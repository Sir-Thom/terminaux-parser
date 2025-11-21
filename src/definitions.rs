#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    Decckm,
    Unknown(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectGraphicRendition {
    Reset,
    Bold,
    Faint,
    Italic,
    Underline,
    BlinkSlow,
    BlinkRapid,
    Reverse,
    Conceal,
    Reveal,
    NotItalic,
    NotUnderline,
    NormalIntensity,
    ForegroundDefault,
    BackgroundDefault,
    // Foreground Colors
    ForegroundBlack, ForegroundRed, ForegroundGreen, ForegroundYellow,
    ForegroundBlue, ForegroundMagenta, ForegroundCyan, ForegroundWhite,
    ForegroundBrightBlack, ForegroundBrightRed, ForegroundBrightGreen, ForegroundBrightYellow,
    ForegroundBrightBlue, ForegroundBrightMagenta, ForegroundBrightCyan, ForegroundBrightWhite,
    Foreground8Bit(u8),
    ForegroundTrueColor(u8, u8, u8),
    // Background Colors
    BackgroundBlack, BackgroundRed, BackgroundGreen, BackgroundYellow,
    BackgroundBlue, BackgroundMagenta, BackgroundCyan, BackgroundWhite,
    BackgroundBrightBlack, BackgroundBrightRed, BackgroundBrightGreen, BackgroundBrightYellow,
    BackgroundBrightBlue, BackgroundBrightMagenta, BackgroundBrightCyan, BackgroundBrightWhite,
    Background8Bit(u8),
    BackgroundTrueColor(u8, u8, u8),
    Unknown(usize),
}

impl SelectGraphicRendition {
    /// Helper to convert basic numeric codes to SGR
    pub fn from_usize(val: usize) -> SelectGraphicRendition {
        match val {
            0 => SelectGraphicRendition::Reset,
            1 => SelectGraphicRendition::Bold,
            2 => SelectGraphicRendition::Faint,
            3 => SelectGraphicRendition::Italic,
            4 => SelectGraphicRendition::Underline,
            5 => SelectGraphicRendition::BlinkSlow,
            6 => SelectGraphicRendition::BlinkRapid,
            7 => SelectGraphicRendition::Reverse,
            8 => SelectGraphicRendition::Conceal,
            22 => SelectGraphicRendition::NormalIntensity,
            23 => SelectGraphicRendition::NotItalic,
            24 => SelectGraphicRendition::NotUnderline,
            28 => SelectGraphicRendition::Reveal,
            30 => SelectGraphicRendition::ForegroundBlack,
            31 => SelectGraphicRendition::ForegroundRed,
            32 => SelectGraphicRendition::ForegroundGreen,
            33 => SelectGraphicRendition::ForegroundYellow,
            34 => SelectGraphicRendition::ForegroundBlue,
            35 => SelectGraphicRendition::ForegroundMagenta,
            36 => SelectGraphicRendition::ForegroundCyan,
            37 => SelectGraphicRendition::ForegroundWhite,
            38 => SelectGraphicRendition::Unknown(38), // Handled in parser logic usually
            39 => SelectGraphicRendition::ForegroundDefault,
            40 => SelectGraphicRendition::BackgroundBlack,
            41 => SelectGraphicRendition::BackgroundRed,
            42 => SelectGraphicRendition::BackgroundGreen,
            43 => SelectGraphicRendition::BackgroundYellow,
            44 => SelectGraphicRendition::BackgroundBlue,
            45 => SelectGraphicRendition::BackgroundMagenta,
            46 => SelectGraphicRendition::BackgroundCyan,
            47 => SelectGraphicRendition::BackgroundWhite,
            48 => SelectGraphicRendition::Unknown(48), // Handled in parser logic usually
            49 => SelectGraphicRendition::BackgroundDefault,
            90 => SelectGraphicRendition::ForegroundBrightBlack,
            91 => SelectGraphicRendition::ForegroundBrightRed,
            92 => SelectGraphicRendition::ForegroundBrightGreen,
            93 => SelectGraphicRendition::ForegroundBrightYellow,
            94 => SelectGraphicRendition::ForegroundBrightBlue,
            95 => SelectGraphicRendition::ForegroundBrightMagenta,
            96 => SelectGraphicRendition::ForegroundBrightCyan,
            97 => SelectGraphicRendition::ForegroundBrightWhite,
            100 => SelectGraphicRendition::BackgroundBrightBlack,
            101 => SelectGraphicRendition::BackgroundBrightRed,
            102 => SelectGraphicRendition::BackgroundBrightGreen,
            103 => SelectGraphicRendition::BackgroundBrightYellow,
            104 => SelectGraphicRendition::BackgroundBrightBlue,
            105 => SelectGraphicRendition::BackgroundBrightMagenta,
            106 => SelectGraphicRendition::BackgroundBrightCyan,
            107 => SelectGraphicRendition::BackgroundBrightWhite,
            _ => SelectGraphicRendition::Unknown(val),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TerminalOutput {
    Data(Vec<u8>),
    /// Operating System Command (e.g., window title).
    /// Format: `command` ID (e.g., 0, 52) and the payload bytes.
    Osc { command: usize, payload: Vec<u8> },
    /// Device Control String (e.g., Sixel images, terminfo requests).
    /// Contains the raw data bytes inside the DCS sequence.
    DeviceControlString {
        params: Vec<usize>,
        intermediates: Vec<u8>,
        data: Vec<u8>
    },
    SetCursorPos { x: Option<usize>, y: Option<usize> },
    CursorUp(usize),
    CursorDown(usize),
    CursorForward(usize),
    CursorBackward(usize),
    ClearForwards,
    ClearAll,
    ClearLineForwards,
    Delete(usize),
    InsertSpaces(usize),
    Backspace,
    Newline,
    CarriageReturn,
    Sgr(SelectGraphicRendition),
    SetCursorVisibility(bool),
    SetMode(Mode),
    ResetMode(Mode),
    EnterAltScreen,
    ExitAltScreen,
    Invalid,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatTag {
    pub start: usize,
    pub end: usize,
    pub blink: bool,
    pub fg_color: SelectGraphicRendition,
    pub bg_color: SelectGraphicRendition,
    pub bold: bool,
    pub italic: bool,
    pub url: Option<String>,
}