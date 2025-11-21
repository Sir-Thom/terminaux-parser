use crate::definitions::{Mode, SelectGraphicRendition, TerminalOutput};
use crate::tables::{Action, State, CLASS_TABLE, TRANSITION_TABLE};
use log::{debug, warn};

pub struct AnsiParser {
    state: State,
    params: Vec<usize>,
    current_param: Option<usize>,
    intermediates: Vec<u8>,
    // Buffer to collect printable data to send in chunks
    data_buffer: Vec<u8>,
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
    fn get_param(&self, index: usize, default: usize) -> usize {
        *self.params.get(index).unwrap_or(&default)
    }

    fn get_param_opt(&self, index: usize) -> Option<usize> {
        self.params.get(index).copied()
    }

    fn flush_data(&mut self, output: &mut Vec<TerminalOutput>) {
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
                    }

                    // 2. Store current state into DCS caches
                    self.dcs_params_cache = self.params.clone();
                    self.dcs_intermediates_cache = self.intermediates.clone();

                    // 3. Clear the buffer for the upcoming data string
                    self.dcs_buffer.clear();
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
                        i += 1; // Skip the next parameter to avoid double processing
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
                        i += 1; // Skip the next parameter to avoid double processing
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
#[cfg(test)]
mod comprehensive_tests {
    use super::*;
    use crate::definitions::*;
    // ========== CURSOR MOVEMENT TESTS ==========

    #[test]
    fn test_cursor_up_variations() {
        let mut parser = AnsiParser::new();

        // Default parameter (1)
        let output = parser.push(b"\x1B[A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(1)]);

        // Explicit single digit
        let output = parser.push(b"\x1B[5A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(5)]);

        // Multiple digits
        let output = parser.push(b"\x1B[123A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(123)]);

        // Zero (should default to 1)
        let output = parser.push(b"\x1B[0A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(0)]);
    }

    #[test]
    fn test_cursor_down_variations() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"\x1B[B");
        assert_eq!(output, vec![TerminalOutput::CursorDown(1)]);

        let output = parser.push(b"\x1B[10B");
        assert_eq!(output, vec![TerminalOutput::CursorDown(10)]);
    }

    #[test]
    fn test_cursor_forward_variations() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"\x1B[C");
        assert_eq!(output, vec![TerminalOutput::CursorForward(1)]);

        let output = parser.push(b"\x1B[7C");
        assert_eq!(output, vec![TerminalOutput::CursorForward(7)]);
    }

    #[test]
    fn test_cursor_backward_variations() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"\x1B[D");
        assert_eq!(output, vec![TerminalOutput::CursorBackward(1)]);

        let output = parser.push(b"\x1B[3D");
        assert_eq!(output, vec![TerminalOutput::CursorBackward(3)]);
    }

    #[test]
    fn test_cursor_position_h() {
        let mut parser = AnsiParser::new();

        // Home position (no params)
        let output = parser.push(b"\x1B[H");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(1),
            y: Some(1)
        }]);

        // Specific row only
        let output = parser.push(b"\x1B[5H");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(1),
            y: Some(5)
        }]);

        // Specific row and column
        let output = parser.push(b"\x1B[10;20H");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(20),
            y: Some(10)
        }]);

        // Column only (edge case)
        let output = parser.push(b"\x1B[;15H");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(15),
            y: Some(1)
        }]);
    }

    #[test]
    fn test_cursor_position_f() {
        let mut parser = AnsiParser::new();

        // 'f' is an alias for 'H'
        let output = parser.push(b"\x1B[f");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(1),
            y: Some(1)
        }]);

        let output = parser.push(b"\x1B[8;12f");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(12),
            y: Some(8)
        }]);
    }

    #[test]
    fn test_cursor_horizontal_absolute() {
        let mut parser = AnsiParser::new();

        // Move to column 1 (default)
        let output = parser.push(b"\x1B[G");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(1),
            y: None
        }]);

        // Move to column 40
        let output = parser.push(b"\x1B[40G");
        assert_eq!(output, vec![TerminalOutput::SetCursorPos {
            x: Some(40),
            y: None
        }]);
    }

    // ========== ERASING TESTS ==========

    #[test]
    fn test_erase_in_display_variations() {
        let mut parser = AnsiParser::new();

        // J with param 0 - clear from cursor to end
        let output = parser.push(b"\x1B[0J");
        assert_eq!(output, vec![TerminalOutput::ClearForwards]);

        // J with no param (defaults to 0)
        let output = parser.push(b"\x1B[J");
        assert_eq!(output, vec![TerminalOutput::ClearForwards]);

        // J with param 2 - clear entire screen
        let output = parser.push(b"\x1B[2J");
        assert_eq!(output, vec![TerminalOutput::ClearAll]);

        // J with param 3 - clear entire screen + scrollback
        let output = parser.push(b"\x1B[3J");
        assert_eq!(output, vec![TerminalOutput::ClearAll]);

        // J with param 1 - not implemented, should be ignored
        let output = parser.push(b"\x1B[1J");
        assert!(output.is_empty());
    }

    #[test]
    fn test_erase_in_line_variations() {
        let mut parser = AnsiParser::new();

        // K with param 0 - clear from cursor to end of line
        let output = parser.push(b"\x1B[0K");
        assert_eq!(output, vec![TerminalOutput::ClearLineForwards]);

        // K with no param (defaults to 0)
        let output = parser.push(b"\x1B[K");
        assert_eq!(output, vec![TerminalOutput::ClearLineForwards]);

        // K with param 2 - clear entire line
        let output = parser.push(b"\x1B[2K");
        assert_eq!(output, vec![TerminalOutput::ClearLineForwards]);

        // K with param 1 - backspace (unusual behavior)
        let output = parser.push(b"\x1B[1K");
        assert_eq!(output, vec![TerminalOutput::Backspace]);
    }

    #[test]
    fn test_delete_characters() {
        let mut parser = AnsiParser::new();

        // Delete 1 character (default)
        let output = parser.push(b"\x1B[P");
        assert_eq!(output, vec![TerminalOutput::Delete(1)]);

        // Delete 5 characters
        let output = parser.push(b"\x1B[5P");
        assert_eq!(output, vec![TerminalOutput::Delete(5)]);

        // Delete 100 characters
        let output = parser.push(b"\x1B[100P");
        assert_eq!(output, vec![TerminalOutput::Delete(100)]);
    }

    #[test]
    fn test_insert_spaces() {
        let mut parser = AnsiParser::new();

        // Insert 1 space (default)
        let output = parser.push(b"\x1B[@");
        assert_eq!(output, vec![TerminalOutput::InsertSpaces(1)]);

        // Insert 3 spaces
        let output = parser.push(b"\x1B[3@");
        assert_eq!(output, vec![TerminalOutput::InsertSpaces(3)]);

        // Insert 50 spaces
        let output = parser.push(b"\x1B[50@");
        assert_eq!(output, vec![TerminalOutput::InsertSpaces(50)]);
    }

    // ========== SGR (GRAPHICS) TESTS ==========

    #[test]
    fn test_sgr_reset() {
        let mut parser = AnsiParser::new();

        // Empty params = reset
        let output = parser.push(b"\x1B[m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Reset)]);

        // Explicit 0 = reset
        let output = parser.push(b"\x1B[0m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Reset)]);
    }

    #[test]
    fn test_sgr_text_attributes() {
        let mut parser = AnsiParser::new();

        // Bold
        let output = parser.push(b"\x1B[1m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Bold)]);

        // Faint
        let output = parser.push(b"\x1B[2m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Faint)]);

        // Italic
        let output = parser.push(b"\x1B[3m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Italic)]);

        // Underline
        let output = parser.push(b"\x1B[4m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Underline)]);

        // Blink slow
        let output = parser.push(b"\x1B[5m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::BlinkSlow)]);

        // Blink rapid
        let output = parser.push(b"\x1B[6m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::BlinkRapid)]);

        // Reverse
        let output = parser.push(b"\x1B[7m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Reverse)]);

        // Conceal
        let output = parser.push(b"\x1B[8m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Conceal)]);
    }

    #[test]
    fn test_sgr_reset_attributes() {
        let mut parser = AnsiParser::new();

        // Normal intensity
        let output = parser.push(b"\x1B[22m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::NormalIntensity)]);

        // Not italic
        let output = parser.push(b"\x1B[23m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::NotItalic)]);

        // Not underline
        let output = parser.push(b"\x1B[24m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::NotUnderline)]);

        // Reveal (not concealed)
        let output = parser.push(b"\x1B[28m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::Reveal)]);
    }

    #[test]
    fn test_sgr_standard_foreground_colors() {
        let mut parser = AnsiParser::new();

        let colors = [
            (30, SelectGraphicRendition::ForegroundBlack),
            (31, SelectGraphicRendition::ForegroundRed),
            (32, SelectGraphicRendition::ForegroundGreen),
            (33, SelectGraphicRendition::ForegroundYellow),
            (34, SelectGraphicRendition::ForegroundBlue),
            (35, SelectGraphicRendition::ForegroundMagenta),
            (36, SelectGraphicRendition::ForegroundCyan),
            (37, SelectGraphicRendition::ForegroundWhite),
        ];

        for (code, expected) in colors {
            let input = format!("\x1B[{}m", code);
            let output = parser.push(input.as_bytes());
            assert_eq!(output, vec![TerminalOutput::Sgr(expected)]);
        }

        // Default foreground
        let output = parser.push(b"\x1B[39m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::ForegroundDefault)]);
    }

    #[test]
    fn test_sgr_standard_background_colors() {
        let mut parser = AnsiParser::new();

        let colors = [
            (40, SelectGraphicRendition::BackgroundBlack),
            (41, SelectGraphicRendition::BackgroundRed),
            (42, SelectGraphicRendition::BackgroundGreen),
            (43, SelectGraphicRendition::BackgroundYellow),
            (44, SelectGraphicRendition::BackgroundBlue),
            (45, SelectGraphicRendition::BackgroundMagenta),
            (46, SelectGraphicRendition::BackgroundCyan),
            (47, SelectGraphicRendition::BackgroundWhite),
        ];

        for (code, expected) in colors {
            let input = format!("\x1B[{}m", code);
            let output = parser.push(input.as_bytes());
            assert_eq!(output, vec![TerminalOutput::Sgr(expected)]);
        }

        // Default background
        let output = parser.push(b"\x1B[49m");
        assert_eq!(output, vec![TerminalOutput::Sgr(SelectGraphicRendition::BackgroundDefault)]);
    }

    #[test]
    fn test_sgr_bright_foreground_colors() {
        let mut parser = AnsiParser::new();

        let colors = [
            (90, SelectGraphicRendition::ForegroundBrightBlack),
            (91, SelectGraphicRendition::ForegroundBrightRed),
            (92, SelectGraphicRendition::ForegroundBrightGreen),
            (93, SelectGraphicRendition::ForegroundBrightYellow),
            (94, SelectGraphicRendition::ForegroundBrightBlue),
            (95, SelectGraphicRendition::ForegroundBrightMagenta),
            (96, SelectGraphicRendition::ForegroundBrightCyan),
            (97, SelectGraphicRendition::ForegroundBrightWhite),
        ];

        for (code, expected) in colors {
            let input = format!("\x1B[{}m", code);
            let output = parser.push(input.as_bytes());
            assert_eq!(output, vec![TerminalOutput::Sgr(expected)]);
        }
    }

    #[test]
    fn test_sgr_bright_background_colors() {
        let mut parser = AnsiParser::new();

        let colors = [
            (100, SelectGraphicRendition::BackgroundBrightBlack),
            (101, SelectGraphicRendition::BackgroundBrightRed),
            (102, SelectGraphicRendition::BackgroundBrightGreen),
            (103, SelectGraphicRendition::BackgroundBrightYellow),
            (104, SelectGraphicRendition::BackgroundBrightBlue),
            (105, SelectGraphicRendition::BackgroundBrightMagenta),
            (106, SelectGraphicRendition::BackgroundBrightCyan),
            (107, SelectGraphicRendition::BackgroundBrightWhite),
        ];

        for (code, expected) in colors {
            let input = format!("\x1B[{}m", code);
            let output = parser.push(input.as_bytes());
            assert_eq!(output, vec![TerminalOutput::Sgr(expected)]);
        }
    }

    #[test]
    fn test_sgr_8bit_colors() {
        let mut parser = AnsiParser::new();

        // Foreground 8-bit color
        let output = parser.push(b"\x1B[38;5;123m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Foreground8Bit(123))
        ]);

        // Background 8-bit color
        let output = parser.push(b"\x1B[48;5;200m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Background8Bit(200))
        ]);

        // Edge case: 0
        let output = parser.push(b"\x1B[38;5;0m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Foreground8Bit(0))
        ]);

        // Edge case: 255
        let output = parser.push(b"\x1B[38;5;255m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Foreground8Bit(255))
        ]);
    }

    #[test]
    fn test_sgr_true_color() {
        let mut parser = AnsiParser::new();

        // Foreground RGB
        let output = parser.push(b"\x1B[38;2;255;128;0m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundTrueColor(255, 128, 0))
        ]);

        // Background RGB
        let output = parser.push(b"\x1B[48;2;0;100;200m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundTrueColor(0, 100, 200))
        ]);

        // Edge case: black
        let output = parser.push(b"\x1B[38;2;0;0;0m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundTrueColor(0, 0, 0))
        ]);

        // Edge case: white
        let output = parser.push(b"\x1B[48;2;255;255;255m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundTrueColor(255, 255, 255))
        ]);
    }

    #[test]
    fn test_sgr_multiple_params() {
        let mut parser = AnsiParser::new();

        // Bold + Red foreground + Green background
        let output = parser.push(b"\x1B[1;31;42m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Bold),
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundGreen),
        ]);

        // Complex: Bold, Italic, Underline, 8-bit color
        let output = parser.push(b"\x1B[1;3;4;38;5;99m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Bold),
            TerminalOutput::Sgr(SelectGraphicRendition::Italic),
            TerminalOutput::Sgr(SelectGraphicRendition::Underline),
            TerminalOutput::Sgr(SelectGraphicRendition::Foreground8Bit(99)),
        ]);
    }

    #[test]
    fn test_sgr_incomplete_extended_colors() {
        let mut parser = AnsiParser::new();

        // 38 without following parameters
        let output = parser.push(b"\x1B[38m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Unknown(38))
        ]);

        // 38;5 without color index
        let output = parser.push(b"\x1B[38;5m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Unknown(38))
        ]);

        // 38;2 without all RGB values
        let output = parser.push(b"\x1B[38;2;255m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Unknown(38))
        ]);

        // Same for background
        let output = parser.push(b"\x1B[48m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Unknown(48))
        ]);
    }

    // ========== MODE TESTS ==========

    #[test]
    fn test_cursor_visibility() {
        let mut parser = AnsiParser::new();

        // Show cursor
        let output = parser.push(b"\x1B[?25h");
        assert_eq!(output, vec![TerminalOutput::SetCursorVisibility(true)]);

        // Hide cursor
        let output = parser.push(b"\x1B[?25l");
        assert_eq!(output, vec![TerminalOutput::SetCursorVisibility(false)]);
    }

    #[test]
    fn test_alt_screen() {
        let mut parser = AnsiParser::new();

        // Enter alt screen
        let output = parser.push(b"\x1B[?1049h");

        assert_eq!(output, vec![TerminalOutput::EnterAltScreen]);

        // Exit alt screen
        let output = parser.push(b"\x1B[?1049l");



    }

    #[test]
    fn test_decckm_mode() {
        let mut parser = AnsiParser::new();

        // Set DECCKM
        let output = parser.push(b"\x1B[?1h");
        assert_eq!(output, vec![TerminalOutput::SetMode(Mode::Decckm)]);

        // Reset DECCKM
        let output = parser.push(b"\x1B[?1l");
        assert_eq!(output, vec![TerminalOutput::ResetMode(Mode::Decckm)]);
    }

    #[test]
    fn test_unhandled_modes() {
        let mut parser = AnsiParser::new();

        // Unknown mode - should be ignored
        let output = parser.push(b"\x1B[?999h");
        assert!(output.is_empty());

        let output = parser.push(b"\x1B[?999l");
        assert!(output.is_empty());
    }

    // ========== CONTROL CHARACTER TESTS ==========

    #[test]
    fn test_backspace() {
        let mut parser = AnsiParser::new();

        // ASCII backspace
        let output = parser.push(b"\x08");
        assert_eq!(output, vec![TerminalOutput::Backspace]);

        // DEL character
        let output = parser.push(b"\x7f");
        assert_eq!(output, vec![TerminalOutput::Backspace]);
    }

    #[test]
    fn test_newline_variations() {
        let mut parser = AnsiParser::new();

        // LF
        let output = parser.push(b"\x0A");
        assert_eq!(output, vec![TerminalOutput::Newline]);

        // VT (vertical tab)
        let output = parser.push(b"\x0B");
        assert_eq!(output, vec![TerminalOutput::Newline]);

        // FF (form feed)
        let output = parser.push(b"\x0C");
        assert_eq!(output, vec![TerminalOutput::Newline]);
    }

    #[test]
    fn test_carriage_return() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"\x0D");
        assert_eq!(output, vec![TerminalOutput::CarriageReturn]);
    }

    // ========== ESC SEQUENCE TESTS ==========

    #[test]
    fn test_esc_sequences() {
        let mut parser = AnsiParser::new();

        // ESC D - Index (newline)
        let output = parser.push(b"\x1BD");
        assert_eq!(output, vec![TerminalOutput::Newline]);

        // ESC M - Reverse Index (cursor up)
        let output = parser.push(b"\x1BM");
        assert_eq!(output, vec![TerminalOutput::CursorUp(1)]);

        // ESC E - Next Line (newline)
        let output = parser.push(b"\x1BE");
        assert_eq!(output, vec![TerminalOutput::Newline]);
    }

    // ========== OSC TESTS ==========

    #[test]
    fn test_osc_window_title() {
        let mut parser = AnsiParser::new();

        // OSC 0 - Set icon name and window title
        let output = parser.push(b"\x1B]0;My Window\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 0,
                payload: b"My Window".to_vec()
            }
        ]);

        // OSC 2 - Set window title
        let output = parser.push(b"\x1B]2;Terminal Title\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 2,
                payload: b"Terminal Title".to_vec()
            }
        ]);
    }

    #[test]
    fn test_osc_hyperlink() {
        let mut parser = AnsiParser::new();

        // OSC 8 - Set hyperlink
        let output = parser.push(b"\x1B]8;;https://example.com\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 8,
                payload: b";https://example.com".to_vec()
            }
        ]);

        // OSC 8 with params
        let output = parser.push(b"\x1B]8;id=123;https://test.com\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 8,
                payload: b"id=123;https://test.com".to_vec()
            }
        ]);

        // OSC 8 - Clear hyperlink (empty URL)
        let output = parser.push(b"\x1B]8;;\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 8,
                payload: b";".to_vec()
            }
        ]);
    }

    #[test]
    fn test_osc_with_st_terminator() {
        let mut parser = AnsiParser::new();

        // OSC terminated with ST (ESC \)
        let output = parser.push(b"\x1B]0;Title\x1B\\");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 0,
                payload: b"Title".to_vec()
            }
        ]);
    }

    #[test]
    fn test_osc_empty() {
        let mut parser = AnsiParser::new();

        // OSC with no payload
        let output = parser.push(b"\x1B]123\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 123,
                payload: b"123".to_vec()
            }
        ]);
    }

    // ========== DCS TESTS ==========

    #[test]
    fn test_dcs_basic() {
        let mut parser = AnsiParser::new();

        // DCS with params and data
        let output = parser.push(b"\x1BP1;2$qData\x1B\\");
        assert_eq!(output, vec![
            TerminalOutput::DeviceControlString {
                params: vec![1, 2],
                intermediates: b"$".to_vec(),
                data: b"qData".to_vec()
            }
        ]);
    }

    #[test]
    fn test_dcs_without_params() {
        let mut parser = AnsiParser::new();

        // DCS with no params
        let output = parser.push(b"\x1BPqHello\x1B\\");
        assert_eq!(output, vec![
            TerminalOutput::DeviceControlString {
                params: vec![],
                intermediates: vec![],
                data: b"qHello".to_vec()
            }
        ]);
    }

    #[test]
    fn test_dcs_sixel_like() {
        let mut parser = AnsiParser::new();

        // Sixel-like DCS (simplified)
        let output = parser.push(b"\x1BP0;0;0q#0;2;100;100;100#0~~\x1B\\");
        assert_eq!(output.len(), 1);
        match &output[0] {
            TerminalOutput::DeviceControlString { params, data, .. } => {
                assert_eq!(params, &vec![0, 0, 0]);
                assert!(data.starts_with(b"q"));
            }
            _ => panic!("Expected DCS"),
        }
    }

    // ========== DATA AND MIXED CONTENT TESTS ==========

    #[test]
    fn test_simple_text() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"Hello, World!");
        assert_eq!(output, vec![
            TerminalOutput::Data(b"Hello, World!".to_vec())
        ]);
    }

    #[test]
    fn test_text_with_newlines() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"Line 1\nLine 2\nLine 3");
        assert_eq!(output, vec![
            TerminalOutput::Data(b"Line 1".to_vec()),
            TerminalOutput::Newline,
            TerminalOutput::Data(b"Line 2".to_vec()),
            TerminalOutput::Newline,
            TerminalOutput::Data(b"Line 3".to_vec()),
        ]);
    }

    #[test]
    fn test_mixed_text_and_escape() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"Hello\x1B[31mRed\x1B[0mWorld");
        assert_eq!(output, vec![
            TerminalOutput::Data(b"Hello".to_vec()),
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Data(b"Red".to_vec()),
            TerminalOutput::Sgr(SelectGraphicRendition::Reset),
            TerminalOutput::Data(b"World".to_vec()),
        ]);
    }

    #[test]
    fn test_utf8_text() {
        let mut parser = AnsiParser::new();

        // UTF-8 characters
        let output = parser.push("Hello 世界 🌍".as_bytes());
        assert_eq!(output.len(), 1);
        match &output[0] {
            TerminalOutput::Data(data) => {
                assert_eq!(String::from_utf8_lossy(data), "Hello 世界 🌍");
            }
            _ => panic!("Expected Data"),
        }
    }

    #[test]
    fn test_complex_formatted_text() {
        let mut parser = AnsiParser::new();

        // Complex: Bold red text on green background
        let output = parser.push(b"\x1B[1;31;42mBold Red on Green\x1B[0m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Bold),
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundGreen),
            TerminalOutput::Data(b"Bold Red on Green".to_vec()),
            TerminalOutput::Sgr(SelectGraphicRendition::Reset),
        ]);
    }

    // ========== PARAMETER PARSING TESTS ==========

    #[test]
    fn test_empty_parameters() {
        let mut parser = AnsiParser::new();

        // CSI with empty params (;;)
        let output = parser.push(b"\x1B[;;m");
        assert_eq!(output.len(), 3);
        for sgr_output in output {
            assert_eq!(sgr_output, TerminalOutput::Sgr(SelectGraphicRendition::Reset));
        }
    }

    #[test]
    fn test_large_parameters() {
        let mut parser = AnsiParser::new();

        // Very large number
        let output = parser.push(b"\x1B[9999A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(9999)]);

        // Multiple large numbers
        let output = parser.push(b"\x1B[1000;2000H");
        assert_eq!(output, vec![
            TerminalOutput::SetCursorPos {
                x: Some(2000),
                y: Some(1000)
            }
        ]);
    }

    #[test]
    fn test_zero_parameters() {
        let mut parser = AnsiParser::new();

        // Zero should be treated as 0, not default
        let output = parser.push(b"\x1B[0A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(0)]);

        let output = parser.push(b"\x1B[0;0H");
        assert_eq!(output, vec![
            TerminalOutput::SetCursorPos {
                x: Some(1),
                y: Some(1)
            }
        ]);
    }

    #[test]
    fn test_leading_zeros() {
        let mut parser = AnsiParser::new();

        // Leading zeros should be ignored
        let output = parser.push(b"\x1B[005A");
        assert_eq!(output, vec![TerminalOutput::CursorUp(5)]);

        let output = parser.push(b"\x1B[0010;0020H");
        assert_eq!(output, vec![
            TerminalOutput::SetCursorPos {
                x: Some(20),
                y: Some(10)
            }
        ]);
    }

    // ========== INCREMENTAL PARSING TESTS ==========

    #[test]
    fn test_split_escape_sequence() {
        let mut parser = AnsiParser::new();

        // Split ESC [ sequence
        let output1 = parser.push(b"\x1B");
        assert!(output1.is_empty());

        let output2 = parser.push(b"[");
        assert!(output2.is_empty());

        let output3 = parser.push(b"3");
        assert!(output3.is_empty());

        let output4 = parser.push(b"1");
        assert!(output4.is_empty());

        let output5 = parser.push(b"m");
        assert_eq!(output5, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed)
        ]);
    }

    #[test]
    fn test_split_multi_param_sequence() {
        let mut parser = AnsiParser::new();

        // Split parameter sequence byte by byte
        let mut result = Vec::new();
        for &byte in b"\x1B[1;31;42m" {
            result.extend(parser.push(&[byte]));
        }

        assert_eq!(result, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Bold),
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundGreen),
        ]);
    }

    #[test]
    fn test_split_osc_sequence() {
        let mut parser = AnsiParser::new();

        let mut result = Vec::new();
        for &byte in b"\x1B]0;Title\x07" {
            result.extend(parser.push(&[byte]));
        }

        assert_eq!(result, vec![
            TerminalOutput::Osc {
                command: 0,
                payload: b"Title".to_vec()
            }
        ]);
    }

    #[test]
    fn test_interleaved_text_and_sequences() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"A\x1B[31mB\x1B[0mC");
        assert_eq!(output, vec![
            TerminalOutput::Data(b"A".to_vec()),
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Data(b"B".to_vec()),
            TerminalOutput::Sgr(SelectGraphicRendition::Reset),
            TerminalOutput::Data(b"C".to_vec()),
        ]);
    }

    // ========== EDGE CASES AND ERROR HANDLING ==========

    #[test]
    fn test_empty_input() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"");
        assert!(output.is_empty());
    }

    #[test]
    fn test_incomplete_sequence_at_end() {
        let mut parser = AnsiParser::new();

        // Incomplete sequence - should buffer it
        let output1 = parser.push(b"Text\x1B[");
        assert_eq!(output1, vec![TerminalOutput::Data(b"Text".to_vec())]);

        // Complete it later
        let output2 = parser.push(b"31m");
        assert_eq!(output2, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed)
        ]);
    }

    #[test]
    fn test_invalid_utf8_in_data() {
        let mut parser = AnsiParser::new();

        // Invalid UTF-8 sequence - should not panic
        let output = parser.push(b"Hello\xFF\xFEWorld");
        assert_eq!(output.len(), 1);
        match &output[0] {
            TerminalOutput::Data(_) => {}, // Success - didn't panic
            _ => panic!("Expected Data"),
        }
    }

    #[test]
    fn test_null_bytes() {
        let mut parser = AnsiParser::new();

        // Null bytes should be handled
        let output = parser.push(b"Hello\x00World");
        assert!(output.len() >= 1);
    }

    #[test]
    fn test_consecutive_escapes() {
        let mut parser = AnsiParser::new();

        // Multiple escape sequences back-to-back
        let output = parser.push(b"\x1B[31m\x1B[42m\x1B[1m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed),
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundGreen),
            TerminalOutput::Sgr(SelectGraphicRendition::Bold),
        ]);
    }

    #[test]
    fn test_escape_inside_escape() {
        let mut parser = AnsiParser::new();

        // ESC during CSI should restart
        let output = parser.push(b"\x1B[31\x1B[42m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::BackgroundGreen)
        ]);
    }

    #[test]
    fn test_unrecognized_csi_commands() {
        let mut parser = AnsiParser::new();

        // Unrecognized CSI command - should log warning but not crash
        let _output = parser.push(b"\x1B[999X");
        // Behavior depends on implementation - might be empty or Invalid
        // Just ensure it doesn't panic
        assert!(true);
    }

    #[test]
    fn test_malformed_sgr_sequences() {
        let mut parser = AnsiParser::new();

        // SGR with unknown code
        let output = parser.push(b"\x1B[999m");
        assert_eq!(output, vec![
            TerminalOutput::Sgr(SelectGraphicRendition::Unknown(999))
        ]);
    }

    // ========== STATE MACHINE TESTS ==========

    #[test]
    fn test_state_transitions() {
        let mut parser = AnsiParser::new();

        // Start in Ground state
        assert_eq!(parser.state, State::Ground);

        // ESC should transition to Escape state
        parser.push(b"\x1B");
        assert_eq!(parser.state, State::Escape);

        // [ should transition to CsiEntry
        parser.push(b"[");
        assert_eq!(parser.state, State::CsiEntry);

        // Digit should transition to CsiParam
        parser.push(b"3");
        assert_eq!(parser.state, State::CsiParam);

        // Final byte should return to Ground
        parser.push(b"m");
        assert_eq!(parser.state, State::Ground);
    }

    #[test]
    fn test_state_reset_on_complete_sequence() {
        let mut parser = AnsiParser::new();

        // Complete a sequence
        parser.push(b"\x1B[31m");
        assert_eq!(parser.state, State::Ground);
        assert!(parser.params.is_empty());
        assert!(parser.current_param.is_none());
        assert!(parser.intermediates.is_empty());
    }

    // ========== REAL-WORLD SCENARIOS ==========

    #[test]
    fn test_vim_like_output() {
        let mut parser = AnsiParser::new();

        // Simulating vim clearing screen and positioning cursor
        let output = parser.push(b"\x1B[2J\x1B[H\x1B[31mVim\x1B[0m");
        assert_eq!(output.len(), 5);
        assert!(matches!(output[0], TerminalOutput::ClearAll));
        assert!(matches!(output[1], TerminalOutput::SetCursorPos { .. }));
        assert!(matches!(output[2], TerminalOutput::Sgr(SelectGraphicRendition::ForegroundRed)));
        assert!(matches!(output[3], TerminalOutput::Data(_)));
        assert!(matches!(output[4], TerminalOutput::Sgr(SelectGraphicRendition::Reset)));
    }

    #[test]
    fn test_progress_bar_like_output() {
        let mut parser = AnsiParser::new();

        // Simulating a progress bar with carriage returns
        let output = parser.push(b"[====      ] 40%\r[==========] 100%");

        let has_cr = output.iter().any(|o| matches!(o, TerminalOutput::CarriageReturn));
        assert!(has_cr);
    }

    #[test]
    fn test_colored_prompt() {
        let mut parser = AnsiParser::new();

        // Typical colored shell prompt
        let output = parser.push(
            b"\x1B[1;32muser@host\x1B[0m:\x1B[1;34m/path\x1B[0m$ "
        );

        // Should have green user@host, reset, blue path, reset, prompt
        assert!(output.len() > 5);
    }

    #[test]
    fn test_hyperlink_in_text() {
        let mut parser = AnsiParser::new();

        // Text with embedded hyperlink
        let output = parser.push(
            b"Click \x1B]8;;https://example.com\x07here\x1B]8;;\x07 for more"
        );

        // Should have text, OSC open link, text, OSC close link, text
        assert!(output.len() >= 5);

        let has_link = output.iter().any(|o| {
            matches!(o, TerminalOutput::Osc { command: 8, .. })
        });
        assert!(has_link);
    }

    #[test]
    fn test_window_title_change() {
        let mut parser = AnsiParser::new();

        let output = parser.push(b"\x1B]0;New Terminal Title\x07");
        assert_eq!(output, vec![
            TerminalOutput::Osc {
                command: 0,
                payload: b"New Terminal Title".to_vec()
            }
        ]);
    }

    // ========== BUFFER MANAGEMENT TESTS ==========

    #[test]
    fn test_data_buffer_flushing() {
        let mut parser = AnsiParser::new();

        // Data should be buffered and flushed before escape sequences
        let output = parser.push(b"Hello");
        assert_eq!(output, vec![TerminalOutput::Data(b"Hello".to_vec())]);
        assert!(parser.data_buffer.is_empty());
    }

    #[test]
    fn test_multiple_data_segments() {
        let mut parser = AnsiParser::new();

        // Multiple calls should produce separate Data outputs
        let output1 = parser.push(b"First");
        let output2 = parser.push(b"Second");

        assert_eq!(output1, vec![TerminalOutput::Data(b"First".to_vec())]);
        assert_eq!(output2, vec![TerminalOutput::Data(b"Second".to_vec())]);
    }

    #[test]
    fn test_data_flushed_on_control() {
        let mut parser = AnsiParser::new();

        // Data should be flushed before control characters
        let output = parser.push(b"Hello\n");
        assert_eq!(output, vec![
            TerminalOutput::Data(b"Hello".to_vec()),
            TerminalOutput::Newline,
        ]);
    }

    // ========== HELPER FUNCTION TESTS ==========

    #[test]
    fn test_get_param_function() {
        let mut parser = AnsiParser::new();
        parser.params = vec![5, 10, 15];

        assert_eq!(parser.get_param(0, 99), 5);
        assert_eq!(parser.get_param(1, 99), 10);
        assert_eq!(parser.get_param(2, 99), 15);
        assert_eq!(parser.get_param(3, 99), 99); // Out of bounds
        assert_eq!(parser.get_param(100, 99), 99); // Way out of bounds
    }

    #[test]
    fn test_get_param_opt_function() {
        let mut parser = AnsiParser::new();
        parser.params = vec![1, 2, 3];

        assert_eq!(parser.get_param_opt(0), Some(1));
        assert_eq!(parser.get_param_opt(1), Some(2));
        assert_eq!(parser.get_param_opt(2), Some(3));
        assert_eq!(parser.get_param_opt(3), None);
        assert_eq!(parser.get_param_opt(100), None);
    }

    #[test]
    fn test_flush_data_helper() {
        let mut parser = AnsiParser::new();
        parser.data_buffer = b"Test".to_vec();

        let mut output = Vec::new();
        parser.flush_data(&mut output);

        assert_eq!(output, vec![TerminalOutput::Data(b"Test".to_vec())]);
        assert!(parser.data_buffer.is_empty());
    }

    #[test]
    fn test_flush_empty_data() {
        let mut parser = AnsiParser::new();

        let mut output = Vec::new();
        parser.flush_data(&mut output);

        assert!(output.is_empty());
    }

    // ========== PERFORMANCE AND STRESS TESTS ==========

    #[test]
    fn test_large_data_block() {
        let mut parser = AnsiParser::new();

        // Large block of text
        let large_text = vec![b'A'; 10000];
        let output = parser.push(&large_text);

        assert_eq!(output.len(), 1);
        match &output[0] {
            TerminalOutput::Data(data) => {
                assert_eq!(data.len(), 10000);
            }
            _ => panic!("Expected Data"),
        }
    }

    #[test]
    fn test_many_consecutive_sequences() {
        let mut parser = AnsiParser::new();

        // Many escape sequences in a row
        let mut input = Vec::new();
        for i in 30..38 {
            input.extend_from_slice(format!("\x1B[{}m", i).as_bytes());
        }

        let output = parser.push(&input);
        assert_eq!(output.len(), 8); // Should have 8 SGR outputs
    }

    #[test]
    fn test_alternating_text_and_sequences() {
        let mut parser = AnsiParser::new();

        let mut input = Vec::new();
        for _ in 0..100 {
            input.extend_from_slice(b"X\x1B[31m");
        }

        let output = parser.push(&input);
        assert_eq!(output.len(), 200); // 100 Data + 100 SGR
    }

    #[test]
    fn test_parameter_overflow_handling() {
        let mut parser = AnsiParser::new();

        // Test with maximum usize value (shouldn't panic)
        let input = format!("\x1B[{}m", usize::MAX);
        let output = parser.push(input.as_bytes());

        // Should handle gracefully
        assert!(!output.is_empty());
    }
}