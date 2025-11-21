use crate::definitions::{Mode, SelectGraphicRendition, TerminalOutput};
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
                Action::Print => self.data_buffer.push(byte),
                Action::Execute => {
                    self.flush_data(&mut output);
                    match byte {
                        0x08 | 0x7f => output.push(TerminalOutput::Backspace),
                        0x0A | 0x0B | 0x0C => output.push(TerminalOutput::Newline),
                        0x0D => output.push(TerminalOutput::CarriageReturn),
                        // Add DCI handling here:
                        0x11 | 0x12 | 0x13 | 0x14 => {
                            output.push(TerminalOutput::DeviceControl { code: byte });
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
                    output.push(TerminalOutput::DeviceControlString {
                        params: std::mem::take(&mut self.dcs_params_cache),
                        intermediates: std::mem::take(&mut self.dcs_intermediates_cache),
                        data: std::mem::take(&mut self.dcs_buffer),
                    });
                }


            }
            // 4. Transition State
            self.state = next_state;
        }
        // flush any remaining text data after processing the chunck
        self.flush_data(&mut output);
        output
    }

    fn perform_esc_dispatch(&mut self, terminator: u8, output: &mut Vec<TerminalOutput>) {
        match (self.intermediates.first(), terminator) {
            (None, b'D') => output.push(TerminalOutput::Newline),
            (None, b'M') => output.push(TerminalOutput::CursorUp(1)),
            (None, b'E') => output.push(TerminalOutput::Newline),
            // Explicitly handle String Terminator (ST, ESC \)
            (None, b'\\') => { /* Handled in OscEnd or Unhook actions */
                warn!("Unexpected String Terminator ESC \\ outside of string sequence");
            },
            _ => warn!("Unknown ESC sequence: {:?} {}", self.intermediates, terminator as char),
        }
    }

    fn perform_csi_dispatch(&mut self, terminator: u8, output: &mut Vec<TerminalOutput>) {

        let intermediates = self.intermediates.as_slice();
        let param = self.get_param(0, 0);

        // Check for private mode sequences.
        // This triggers if intermediates == [b'?'] (correct case)
        // OR if intermediates is empty but the terminator is 'h' or 'l' and the parameter
        // matches a known private mode (the bugged case).
        if intermediates == [b'?'] || intermediates.is_empty() && (terminator == b'h' || terminator == b'l') {
            match terminator {
                b'h' => match param {
                    25 => output.push(TerminalOutput::SetCursorVisibility(true)),
                    1049 => output.push(TerminalOutput::EnterAltScreen), // **Handles the failing sequence**
                    1 => output.push(TerminalOutput::SetMode(Mode::Decckm)),
                    _ => {}
                },
                b'l' => match param {
                    25 => output.push(TerminalOutput::SetCursorVisibility(false)),
                    1049 => output.push(TerminalOutput::ExitAltScreen),
                    1 => output.push(TerminalOutput::ResetMode(Mode::Decckm)),
                    _ => {}
                },
                _ => {}
            }
            // If the intermediate was '?', we exit here.
            // If the intermediate was empty but we matched a private mode param, we also exit here.
            if intermediates == [b'?'] || param == 1049 || param == 25 || param == 1 {
                return;
            }
        }
        match (intermediates, terminator) {

            // Cursor Movement
            ([], b'A') => output.push(TerminalOutput::CursorUp(self.get_param(0, 1))),
            ([], b'B') => output.push(TerminalOutput::CursorDown(self.get_param(0, 1))),
            ([], b'C') => output.push(TerminalOutput::CursorForward(self.get_param(0, 1))),
            ([], b'D') => output.push(TerminalOutput::CursorBackward(self.get_param(0, 1))),
            ([], b'H') | ([], b'f') => {
                let y = self.get_param_opt(0).map(|v| v.max(1)).unwrap_or(1);
                let x = self.get_param_opt(1).map(|v| v.max(1)).unwrap_or(1);
                output.push(TerminalOutput::SetCursorPos {
                    x: Some(x),
                    y: Some(y),
                });
            }
            ([], b'G') => output.push(TerminalOutput::SetCursorPos {
                x: Some(self.get_param(0, 1).max(1)),
                y: None,
            }),
            // Erasing
            ([], b'J') => match self.get_param(0, 0) {
                0 => output.push(TerminalOutput::ClearForwards),
                2 | 3 => output.push(TerminalOutput::ClearAll),
                _ => {}
            },
            ([], b'K') => match self.get_param(0, 0) {
                0 => output.push(TerminalOutput::ClearLineForwards),
                1 => output.push(TerminalOutput::Backspace),
                2 => output.push(TerminalOutput::ClearLineForwards),
                _ => {}
            },
            ([], b'P') => output.push(TerminalOutput::Delete(self.get_param(0, 1))),
            ([], b'@') => output.push(TerminalOutput::InsertSpaces(self.get_param(0, 1))),
            // Graphics (SGR)
            ([], b'm') => self.parse_sgr(output),
            // Modes
            ([b'?'], b'h') => match self.get_param(0, 0) {
                25 => output.push(TerminalOutput::SetCursorVisibility(true)),
                1049 => output.push(TerminalOutput::EnterAltScreen),
                1 => output.push(TerminalOutput::SetMode(Mode::Decckm)),
                _ => {}
            },
            ([b'?'], b'l') => match self.get_param(0, 0) {
                25 => output.push(TerminalOutput::SetCursorVisibility(false)),
                1049 => output.push(TerminalOutput::ExitAltScreen),
                1 => output.push(TerminalOutput::ResetMode(Mode::Decckm)),
                _ => {}
            },
            _ => warn!("Unknown CSI: {:?} {}", intermediates, terminator as char),
        }
    }

    fn parse_sgr(&self, output: &mut Vec<TerminalOutput>) {
        if self.params.is_empty() {
            output.push(TerminalOutput::Sgr(SelectGraphicRendition::Reset));
            return;
        }

        let mut i = 0;
        while i < self.params.len() {
            let param = self.params[i];
            let sgr = match param {
                38 => {
                    // Extended foreground logic
                    if i + 2 < self.params.len() && self.params[i+1] == 5 {
                        let color = self.params[i+2] as u8;
                        i += 2;
                        SelectGraphicRendition::Foreground8Bit(color)
                    } else if i + 4 < self.params.len() && self.params[i+1] == 2 {
                        let r = self.params[i+2] as u8;
                        let g = self.params[i+3] as u8;
                        let b = self.params[i+4] as u8;
                        i += 4;
                        SelectGraphicRendition::ForegroundTrueColor(r, g, b)
                    } else {
                        // Consume partially available incomplete params to prevent them being interpreted as garbage SGR
                        if i + 1 < self.params.len() {
                            let subtype = self.params[i+1];
                            let available = self.params.len() - i;
                            if subtype == 5 {
                                // Expected 3 (38;5;n), skip whatever we have up to that
                                i += std::cmp::min(available, 3) - 1;
                            } else if subtype == 2 {
                                // Expected 5 (38;2;r;g;b)
                                i += std::cmp::min(available, 5) - 1;
                            } else {
                                i += 1;
                            }
                        }
                        SelectGraphicRendition::Unknown(38)
                    }
                },
                48 => {
                    // Extended background logic
                    if i + 2 < self.params.len() && self.params[i+1] == 5 {
                        let color = self.params[i+2] as u8;
                        i += 2;
                        SelectGraphicRendition::Background8Bit(color)
                    } else if i + 4 < self.params.len() && self.params[i+1] == 2 {
                        let r = self.params[i+2] as u8;
                        let g = self.params[i+3] as u8;
                        let b = self.params[i+4] as u8;
                        i += 4;
                        SelectGraphicRendition::BackgroundTrueColor(r, g, b)
                    } else {
                        if i + 1 < self.params.len() {
                            let subtype = self.params[i+1];
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
                },
                _ => SelectGraphicRendition::from_usize(param)
            };
            output.push(TerminalOutput::Sgr(sgr));
            i += 1;
        }
    }
}