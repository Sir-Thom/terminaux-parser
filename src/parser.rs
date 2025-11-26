use crate::definitions::{CharsetIndex, Mode, SelectGraphicRendition, StandardCharset, TerminalOutput};
use crate::tables::{Action, State, CLASS_TABLE, TRANSITION_TABLE};
use log::{debug, warn};

pub struct AnsiParser {
    pub(crate) state: State,
    pub(crate) params: Vec<usize>,
    pub(crate) current_param: Option<usize>,
    pub(crate) intermediates: Vec<u8>,
    // Buffer to collect printable data to send in chunks
    pub(crate) data_buffer: Vec<u8>,
    // Buffers for string sequences
    osc_buffer: Vec<u8>,
    dcs_buffer: Vec<u8>,
    dcs_params_cache: Vec<usize>,
    dcs_intermediates_cache: Vec<u8>,
    // Character set state
    active_charset: CharsetIndex,
    charsets: [StandardCharset; 4],
    // Synchronized update state
    sync_update_depth: usize,
    sync_buffer: Vec<TerminalOutput>,
    // Preceding character for repeat
    preceding_char: Option<char>,
}

impl Default for AnsiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiParser {
    pub fn new() -> AnsiParser {
        AnsiParser {
            state: State::Ground,
            params: Vec::with_capacity(8),
            current_param: None,
            intermediates: Vec::with_capacity(4),
            data_buffer: Vec::with_capacity(256),
            osc_buffer: Vec::with_capacity(256),
            dcs_buffer: Vec::with_capacity(256),
            dcs_params_cache: Vec::with_capacity(8),
            dcs_intermediates_cache: Vec::with_capacity(4),
            active_charset: CharsetIndex::G0,
            charsets: [StandardCharset::Ascii; 4],
            sync_update_depth: 0,
            sync_buffer: Vec::new(),
            preceding_char: None,
        }
    }
    // helper for parameter retrieval
    pub(crate) fn get_param(&self, index: usize, default: usize) -> usize {
        *self.params.get(index).unwrap_or(&default)
    }

    pub(crate) fn get_param_opt(&self, index: usize) -> Option<usize> {
        self.params.get(index).copied()
    }

    pub(crate) fn flush_data(&mut self, output: &mut Vec<TerminalOutput>) {
        if !self.data_buffer.is_empty() {
            output.push(TerminalOutput::Data(std::mem::take(&mut self.data_buffer)));
        }
    }
    /// Emit output, respecting synchronized update mode
    fn emit_output(&mut self, output: &mut Vec<TerminalOutput>, item: TerminalOutput) {
        if self.sync_update_depth > 0 {
            self.sync_buffer.push(item);
        } else {
            output.push(item);
        }
    }

    /// Flush synchronized update buffer
    fn flush_sync_buffer(&mut self, output: &mut Vec<TerminalOutput>) {
        if !self.sync_buffer.is_empty() {
            output.append(&mut self.sync_buffer);
        }
    }

    fn clear_state(&mut self) {
        self.params.clear();
        self.current_param = None;
        self.intermediates.clear();
    }

    pub fn push(&mut self, incoming: &[u8]) -> Vec<TerminalOutput> {
        let mut output = Vec::new();

        for &byte in incoming {
            // 1. Classify Byte
            let class_idx = CLASS_TABLE[byte as usize] as usize;
            //2. lookup Transition
            let entry = TRANSITION_TABLE[self.state as usize][class_idx];
            // Unpack
            let next_state_u8 = entry >> 4;
            let action_u8 = entry & 0x0F;

            // Safety: The tables are constructed with strict repr(u8) and bounds
            let next_state: State = unsafe { std::mem::transmute(next_state_u8) };
            let action: Action = unsafe { std::mem::transmute(action_u8) };

            // 3. Perform Action
            match action {
                Action::None | Action::Ignore => {}
                Action::Print => {
                    // Apply charset mapping
                    if let Ok(c) = std::str::from_utf8(&[byte]) {
                        if let Some(ch) = c.chars().next() {
                            let mapped = self.map_char(ch);
                            self.data_buffer.extend(mapped.to_string().as_bytes());
                            self.preceding_char = Some(ch);
                        }
                    } else {
                        self.data_buffer.push(byte);
                    }
                }
                Action::Execute => {
                    self.flush_data(&mut output);
                    match byte {
                        0x08 | 0x7f => self.emit_output(&mut output, TerminalOutput::Backspace),
                        0x0A | 0x0B | 0x0C => self.emit_output(&mut output, TerminalOutput::Newline),
                        0x0D => self.emit_output(&mut output, TerminalOutput::CarriageReturn),
                        0x0E => {
                            // Shift Out - activate G1
                            self.active_charset = CharsetIndex::G1;
                            self.emit_output(
                                &mut output,
                                TerminalOutput::SetActiveCharset(CharsetIndex::G1),
                            );
                        }
                        0x0F => {
                            // Shift In - activate G0
                            self.active_charset = CharsetIndex::G0;
                            self.emit_output(
                                &mut output,
                                TerminalOutput::SetActiveCharset(CharsetIndex::G0),
                            );
                        }
                        0x11 | 0x12 | 0x13 | 0x14 => {
                            self.emit_output(&mut output, TerminalOutput::DeviceControl { code: byte });
                        }
                        _ => debug!("Unhandled C0 execute: {:02X}", byte),
                    }
                }
                Action::Clear => {
                    self.flush_data(&mut output);
                    self.clear_state();
                }
                Action::Collect => self.intermediates.push(byte),
                Action::Param => {
                    // If byte is ';', push the current param and reset
                    if byte == b';' {
                        self.params.push(self.current_param.unwrap_or(0));
                        self.current_param = None;
                    } else if byte.is_ascii_digit() {
                        let digit = (byte - b'0') as usize;
                        self.current_param = Some(
                            self.current_param
                                .unwrap_or(0)
                                .saturating_mul(10)
                                .saturating_add(digit),
                        );
                    }
                }
                Action::EscDispatch => {
                    self.flush_data(&mut output);
                    self.perform_esc_dispatch(byte, &mut output);
                    self.clear_state();
                }
                Action::CsiDispatch => {
                    self.flush_data(&mut output);
                    // Push the last parameter if it exists
                    if let Some(p) = self.current_param {
                        self.params.push(p);
                    } else if !self.params.is_empty() {
                        // Handle edge case: [;;m where last param is implicit default 0
                        self.params.push(0);
                    }
                    self.perform_csi_dispatch(byte, &mut output);
                    self.clear_state();
                }
                // Reset OSC buffer if we were to implement OSC collection
                Action::OscStart => {
                    self.flush_data(&mut output);
                    self.osc_buffer.clear();
                }

                Action::OscPut => {
                    // Collect OSC string chars
                    self.osc_buffer.push(byte);
                }

                Action::OscEnd => {
                    // OSC format is usually: <Int>;<Text>
                    // Example: \x1b]0;Terminal Title\x07
                    if !self.osc_buffer.is_empty() {
                        // Find the separator ';'
                        let (command, payload) = match self.osc_buffer.iter().position(|&b| b == b';') {
                            Some(idx) => {
                                let num_slice = &self.osc_buffer[..idx];
                                let payload_slice = &self.osc_buffer[idx + 1..];

                                // Parse the command number (default to 0 if invalid)
                                let cmd_str = std::str::from_utf8(num_slice).unwrap_or("0");
                                let cmd = cmd_str.parse::<usize>().unwrap_or(0);
                                (cmd, payload_slice.to_vec())
                            }
                            None => {
                                // Edge case: OSC without a payload or separator
                                // Try to parse the entire buffer as command number
                                let cmd_str = std::str::from_utf8(&self.osc_buffer).unwrap_or("0");
                                let cmd = cmd_str.parse::<usize>().unwrap_or(0);
                                (cmd, self.osc_buffer.clone())
                            }
                        };

                        output.push(TerminalOutput::Osc { command, payload });
                    }
                    self.osc_buffer.clear();
                }
                // --- DCS Implementation ---
                Action::Hook => {
                    self.flush_data(&mut output);

                    // 1. Finalize the last parameter being parsed (just like CsiDispatch)
                    if let Some(p) = self.current_param {
                        self.params.push(p);
                    } else if !self.params.is_empty() {
                        // Handle implicit default 0
                        self.params.push(0);
                    }

                    // 2. Store current state into DCS caches
                    self.dcs_params_cache = self.params.clone();
                    self.dcs_intermediates_cache = self.intermediates.clone();

                    // 3. Clear the buffer for the upcoming data string
                    self.dcs_buffer.clear();
                    // Important: The 'byte' here is the Final character (e.g., 'q' or 't').
                    // It is technically part of the data payload start or command identifier.
                    // Tests expect it to be in the data.
                    self.dcs_buffer.push(byte);
                }

                Action::Put => {
                    // Collect the raw data bytes of the DCS string
                    self.dcs_buffer.push(byte);
                }

                Action::Unhook => {
                    // Emit the full package: Params + Intermediates + Data
                    // Take ownership first to avoid multiple mutable borrows
                    let dcs_output = TerminalOutput::DeviceControlString {
                        params: std::mem::take(&mut self.dcs_params_cache),
                        intermediates: std::mem::take(&mut self.dcs_intermediates_cache),
                        data: std::mem::take(&mut self.dcs_buffer),
                    };
                    self.emit_output(&mut output, dcs_output);
                }
            }
            // 4. Transition State
            self.state = next_state;
        }
        // flush any remaining text data after processing the chunk
        self.flush_data(&mut output);
        output
    }

    fn map_char(&self, c: char) -> char {
        let charset = match self.active_charset {
            CharsetIndex::G0 => self.charsets[0],
            CharsetIndex::G1 => self.charsets[1],
            CharsetIndex::G2 => self.charsets[2],
            CharsetIndex::G3 => self.charsets[3],
        };
        charset.map(c)
    }

    fn perform_osc_dispatch(&mut self, output: &mut Vec<TerminalOutput>) {
        if self.osc_buffer.is_empty() {
            return;
        }

        // Find the separator ';'
        let (command, payload) = match self.osc_buffer.iter().position(|&b| b == b';') {
            Some(idx) => {
                let num_slice = &self.osc_buffer[..idx];
                let payload_slice = &self.osc_buffer[idx + 1..];

                // Parse the command number (default to 0 if invalid)
                let cmd_str = std::str::from_utf8(num_slice).unwrap_or("0");
                let cmd = cmd_str.parse::<usize>().unwrap_or(0);
                (cmd, payload_slice.to_vec())
            }
            None => {
                // Edge case: OSC without a payload or separator
                // Try to parse the entire buffer as command number
                let cmd_str = std::str::from_utf8(&self.osc_buffer).unwrap_or("0");
                let cmd = cmd_str.parse::<usize>().unwrap_or(0);
                (cmd, self.osc_buffer.clone())
            }
        };

        // Handle specific OSC commands
        match command {
            // OSC 8 - Hyperlinks
            8 => {
                // Format: OSC 8 ; params ; URI ST
                // If URI is empty, clear hyperlink
                if payload.is_empty() {
                    self.emit_output(output, TerminalOutput::ClearHyperlink);
                } else {
                    // Split params and URI
                    if let Some(semicolon_pos) = payload.iter().position(|&b| b == b';') {
                        let params_slice = &payload[..semicolon_pos];
                        let uri_slice = &payload[semicolon_pos + 1..];

                        // Parse ID from params (format: key=value:key=value)
                        let id = if !params_slice.is_empty() {
                            std::str::from_utf8(params_slice)
                                .ok()
                                .and_then(|s| {
                                    s.split(':')
                                        .find_map(|kv| kv.strip_prefix("id="))
                                        .map(|id| id.to_string())
                                })
                        } else {
                            None
                        };

                        if let Ok(uri) = std::str::from_utf8(uri_slice) {
                            if uri.is_empty() {
                                self.emit_output(output, TerminalOutput::ClearHyperlink);
                            } else {
                                self.emit_output(
                                    output,
                                    TerminalOutput::SetHyperlink {
                                        id,
                                        uri: uri.to_string(),
                                    },
                                );
                            }
                        }
                    }
                }
            }
            _ => {
                // Generic OSC handling
                self.emit_output(output, TerminalOutput::Osc { command, payload });
            }
        }
    }
    fn perform_esc_dispatch(&mut self, terminator: u8, output: &mut Vec<TerminalOutput>) {
        match (self.intermediates.first(), terminator) {
            (None, b'D') => self.emit_output(output, TerminalOutput::Newline),
            (None, b'M') => self.emit_output(output, TerminalOutput::CursorUp(1)),
            (None, b'E') => self.emit_output(output, TerminalOutput::Newline),
            // Explicitly handle String Terminator (ST, ESC \)
            (None, b'\\') => { /* Handled in OscEnd or Unhook actions */
                warn!("Unexpected String Terminator ESC \\ outside of string sequence");
            }
            // Charset designation
            (Some(&b'('), charset) => {
                // Designate G0
                if let Some(cs) = self.parse_charset(charset) {
                    self.charsets[0] = cs;
                    self.emit_output(
                        output,
                        TerminalOutput::ConfigureCharset {
                            index: CharsetIndex::G0,
                            charset: cs,
                        },
                    );
                }
            }
            (Some(&b')'), charset) => {
                // Designate G1
                if let Some(cs) = self.parse_charset(charset) {
                    self.charsets[1] = cs;
                    self.emit_output(
                        output,
                        TerminalOutput::ConfigureCharset {
                            index: CharsetIndex::G1,
                            charset: cs,
                        },
                    );
                }
            }
            (Some(&b'*'), charset) => {
                // Designate G2
                if let Some(cs) = self.parse_charset(charset) {
                    self.charsets[2] = cs;
                    self.emit_output(
                        output,
                        TerminalOutput::ConfigureCharset {
                            index: CharsetIndex::G2,
                            charset: cs,
                        },
                    );
                }
            }
            (Some(&b'+'), charset) => {
                // Designate G3
                if let Some(cs) = self.parse_charset(charset) {
                    self.charsets[3] = cs;
                    self.emit_output(
                        output,
                        TerminalOutput::ConfigureCharset {
                            index: CharsetIndex::G3,
                            charset: cs,
                        },
                    );
                }
            }
            _ => warn!("Unknown ESC sequence: {:?} {}", self.intermediates, terminator as char),
        }
    }

    fn parse_charset(&self, byte: u8) -> Option<StandardCharset> {
        match byte {
            b'B' => Some(StandardCharset::Ascii),
            b'0' => Some(StandardCharset::SpecialCharacterAndLineDrawing),
            _ => None,
        }
    }

    fn perform_csi_dispatch(&mut self, terminator: u8, output: &mut Vec<TerminalOutput>) {

        let has_question_mark = self.intermediates.first() == Some(&b'?');
        let is_empty_intermediates = self.intermediates.is_empty();
        let param = self.get_param(0, 0);

        // Check for private mode sequences.
        if has_question_mark || (is_empty_intermediates && (terminator == b'h' || terminator == b'l')) {
            match terminator {
                b'h' => match param {
                    25 => self.emit_output(output, TerminalOutput::SetCursorVisibility(true)),
                    1049 => self.emit_output(output, TerminalOutput::EnterAltScreen),
                    1 => self.emit_output(output, TerminalOutput::SetMode(Mode::Decckm)),
                    2026 => {
                        output.push(TerminalOutput::BeginSynchronizedUpdate);
                        self.sync_update_depth += 1;
                    }
                    2004 => self.emit_output(output, TerminalOutput::SetMode(Mode::BracketedPaste)),
                    1037 => self.emit_output(output, TerminalOutput::SetMode(Mode::ModifyOtherKeys)),
                    _ => {}
                },
                b'l' => match param {
                    25 => self.emit_output(output, TerminalOutput::SetCursorVisibility(false)),
                    1049 => self.emit_output(output, TerminalOutput::ExitAltScreen),
                    1 => self.emit_output(output, TerminalOutput::ResetMode(Mode::Decckm)),
                    2026 => {
                        if self.sync_update_depth > 0 {
                            self.sync_update_depth -= 1;
                            if self.sync_update_depth == 0 {
                                self.flush_sync_buffer(output);
                            }
                        }
                        output.push(TerminalOutput::EndSynchronizedUpdate);
                    }
                    2004 => self.emit_output(output, TerminalOutput::ResetMode(Mode::BracketedPaste)),
                    1037 => self.emit_output(output, TerminalOutput::ResetMode(Mode::ModifyOtherKeys)),
                    _ => {}
                },
                _ => {}
            }
            if has_question_mark || param == 1049 || param == 25 || param == 1 || param == 2026 {
                return;
            }
        }
        let has_space_intermediate = self.intermediates.first() == Some(&b' ');
        let intermediates_empty = self.intermediates.is_empty();

        match (has_space_intermediate, intermediates_empty, terminator) {
            // Cursor Style
            (true, _, b'q') => {
                use crate::definitions::CursorShape::*;

                let (shape, blinking) = match param {
                    0 | 1 => (Block, true),
                    2 => (Block, false),
                    3 => (Underline, true),
                    4 => (Underline, false),
                    5 => (Beam, true),
                    6 => (Beam, false),
                    _ => (Block, true),
                };

                self.emit_output(output, TerminalOutput::SetCursorStyle { shape, blinking });
            }
            // Character repeat
            (_, true, b'b') => {
                if let Some(ch) = self.preceding_char {
                    let count = self.get_param(0, 1);
                    let repeated: String = std::iter::repeat(ch).take(count).collect();
                    self.emit_output(output, TerminalOutput::Data(repeated.into_bytes()));
                }
            }
            // Cursor Movement
            (_, true, b'A') => self.emit_output(output, TerminalOutput::CursorUp(self.get_param(0, 1))),
            (_, true, b'B') => self.emit_output(output, TerminalOutput::CursorDown(self.get_param(0, 1))),
            (_, true, b'C') => self.emit_output(output, TerminalOutput::CursorForward(self.get_param(0, 1))),
            (_, true, b'D') => self.emit_output(output, TerminalOutput::CursorBackward(self.get_param(0, 1))),
            (_, true, b'H') | (_, true, b'f') => {
                let y = self.get_param_opt(0).map(|v| v.max(1)).unwrap_or(1);
                let x = self.get_param_opt(1).map(|v| v.max(1)).unwrap_or(1);
                self.emit_output(
                    output,
                    TerminalOutput::SetCursorPos {
                        x: Some(x),
                        y: Some(y),
                    },
                );
            }
            (_, true, b'G') => self.emit_output(
                output,
                TerminalOutput::SetCursorPos {
                    x: Some(self.get_param(0, 1).max(1)),
                    y: None,
                },
            ),
            // Erasing
            (_, true, b'J') => match self.get_param(0, 0) {
                0 => self.emit_output(output, TerminalOutput::ClearForwards),
                2 | 3 => self.emit_output(output, TerminalOutput::ClearAll),
                _ => {}
            },
            (_, true, b'K') => match self.get_param(0, 0) {
                0 => self.emit_output(output, TerminalOutput::ClearLineForwards),
                1 => self.emit_output(output, TerminalOutput::Backspace),
                2 => self.emit_output(output, TerminalOutput::ClearLineForwards),
                _ => {}
            },
            (_, true, b'P') => self.emit_output(output, TerminalOutput::Delete(self.get_param(0, 1))),
            (_, true, b'@') => self.emit_output(output, TerminalOutput::InsertSpaces(self.get_param(0, 1))),
            // Graphics (SGR)
            (_, true, b'm') => self.parse_sgr(output),
            // Scrolling region
            (_, true, b'r') => {
                let top = self.get_param(0, 1);
                let bottom = self.get_param_opt(1);
                self.emit_output(output, TerminalOutput::SetScrollingRegion { top, bottom });
            }
            _ => warn!("Unknown CSI: has_space={} empty={} terminator={}",
                      has_space_intermediate, intermediates_empty, terminator as char),
        }
    }

    fn parse_sgr(&mut self, output: &mut Vec<TerminalOutput>) {
        if self.params.is_empty() {
            self.emit_output(output, TerminalOutput::Sgr(SelectGraphicRendition::Reset));
            return;
        }

        let mut i = 0;
        while i < self.params.len() {
            let param = self.params[i];
            let sgr = match param {
                38 => {
                    if i + 2 < self.params.len() && self.params[i + 1] == 5 {
                        let color = self.params[i + 2] as u8;
                        i += 2;
                        SelectGraphicRendition::Foreground8Bit(color)
                    } else if i + 4 < self.params.len() && self.params[i + 1] == 2 {
                        let r = self.params[i + 2] as u8;
                        let g = self.params[i + 3] as u8;
                        let b = self.params[i + 4] as u8;
                        i += 4;
                        SelectGraphicRendition::ForegroundTrueColor(r, g, b)
                    } else {
                        if i + 1 < self.params.len() {
                            let subtype = self.params[i + 1];
                            let available = self.params.len() - i;
                            if subtype == 5 {
                                i += std::cmp::min(available, 3) - 1;
                            } else if subtype == 2 {
                                i += std::cmp::min(available, 5) - 1;
                            } else {
                                i += 1;
                            }
                        }
                        SelectGraphicRendition::Unknown(38)
                    }
                }
                48 => {
                    if i + 2 < self.params.len() && self.params[i + 1] == 5 {
                        let color = self.params[i + 2] as u8;
                        i += 2;
                        SelectGraphicRendition::Background8Bit(color)
                    } else if i + 4 < self.params.len() && self.params[i + 1] == 2 {
                        let r = self.params[i + 2] as u8;
                        let g = self.params[i + 3] as u8;
                        let b = self.params[i + 4] as u8;
                        i += 4;
                        SelectGraphicRendition::BackgroundTrueColor(r, g, b)
                    } else {
                        if i + 1 < self.params.len() {
                            let subtype = self.params[i + 1];
                            let available = self.params.len() - i;
                            if subtype == 5 {
                                i += std::cmp::min(available, 3) - 1;
                            } else if subtype == 2 {
                                i += std::cmp::min(available, 5) - 1;
                            } else {
                                i += 1;
                            }
                        }
                        SelectGraphicRendition::Unknown(48)
                    }
                }
                _ => SelectGraphicRendition::from_usize(param),
            };
            self.emit_output(output, TerminalOutput::Sgr(sgr));
            i += 1;
        }
    }

    // Helper that bypasses sync buffer check (for internal use during sync processing)
    fn emit_output_unchecked(&self, output: &mut Vec<TerminalOutput>, item: TerminalOutput) {
        if self.sync_update_depth > 0 {
            // This is a bit of a hack - we need mutable access but we're in an immutable context
            // In practice, this is only called from parse_sgr which is called with mutable self
            // but the borrow checker doesn't see through it
            // For now, we'll just push directly
            unsafe {
                let self_mut = self as *const Self as *mut Self;
                (*self_mut).sync_buffer.push(item);
            }
        } else {
            output.push(item);
        }
    }
}
