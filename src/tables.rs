#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u8)]
pub enum State {
    Ground = 0,
    Escape = 1,
    EscapeIntermediate = 2,
    CsiEntry = 3,
    CsiParam = 4,
    CsiIntermediate = 5,
    CsiIgnore = 6,
    DcsEntry = 7,
    DcsParam = 8,
    DcsIntermediate = 9,
    DcsPassthrough = 10,
    DcsIgnore = 11,
    OscString = 12,
    SosPmApcString = 13,
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u8)]
pub enum Action {
    None = 0,
    Ignore = 1,
    Print = 2,
    Execute = 3,
    Clear = 4,
    Collect = 5,
    Param = 6,
    EscDispatch = 7,
    CsiDispatch = 8,
    Hook = 9,
    Put = 10,
    Unhook = 11,
    OscStart = 12,
    OscPut = 13,
    OscEnd = 14,
}

pub type TableEntry = u8;

pub const fn pack(state: State, action: Action) -> TableEntry {
    ((state as u8) << 4) | (action as u8)
}

// Classes: 0:Exe, 1:Print, 2:Param, 3:Inter, 4:CsiEntry, 5:Esc, 6:Disp, 7:Osc, 8:Sos, 9:DcsEntry
pub const CLASS_TABLE: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 00-0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, // 10-1F
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, // 20-2F
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, // 30-3F
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, // 40-4F
    9, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 4, 6, 7, 6, 8, // 50-5F ('P' = 0x50 = 9 [DcsEntry])
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, // 60-6F
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 0, // 70-7F
    // 80-FF treated as Print (1)
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

// We use Column 9 for DCS Entry ('P'). 
// For most states, it behaves exactly like Column 6 (Disp).
// Exception: State 1 (Escape), where it triggers DcsEntry.
pub const TRANSITION_TABLE: [[TableEntry; 16]; 14] = [
    // State 0: Ground
    [
        pack(State::Ground, Action::Execute),   // 0:Exe
        pack(State::Ground, Action::Print),     // 1:Print
        pack(State::Ground, Action::Print),     // 2:Param
        pack(State::Ground, Action::Print),     // 3:Inter
        pack(State::Ground, Action::Print),     // 4:CsiEntry
        pack(State::Escape, Action::Clear),     // 5:Esc
        pack(State::Ground, Action::Print),     // 6:Disp
        pack(State::Ground, Action::Print),     // 7:Osc
        pack(State::Ground, Action::Print),     // 8:Sos
        pack(State::Ground, Action::Print),     // 9:DcsEntry (P treated as print)
        0,0,0,0,0,0
    ],
    // State 1: Escape
    [
        pack(State::Escape, Action::Execute),             // 0
        pack(State::Ground, Action::EscDispatch),         // 1
        pack(State::Ground, Action::EscDispatch),         // 2
        pack(State::EscapeIntermediate, Action::Collect), // 3
        pack(State::CsiEntry, Action::Clear),             // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::EscDispatch),         // 6
        pack(State::OscString, Action::OscStart),         // 7
        pack(State::SosPmApcString, Action::None),        // 8
        pack(State::DcsEntry, Action::Clear),             // 9: DcsEntry (Trigger DCS)
        0,0,0,0,0,0
    ],
    // State 2: EscapeIntermediate
    [
        pack(State::EscapeIntermediate, Action::Execute), // 0
        pack(State::Ground, Action::EscDispatch),         // 1
        pack(State::Ground, Action::EscDispatch),         // 2
        pack(State::EscapeIntermediate, Action::Collect), // 3
        pack(State::Ground, Action::EscDispatch),         // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::EscDispatch),         // 6
        pack(State::Ground, Action::Ignore),              // 7
        pack(State::Ground, Action::Ignore),              // 8
        pack(State::Ground, Action::EscDispatch),         // 9: (Treat P as dispatch here)
        0,0,0,0,0,0
    ],
    // State 3: CsiEntry
    [
        pack(State::CsiEntry, Action::Execute),           // 0
        pack(State::CsiIgnore, Action::None),             // 1
        pack(State::CsiParam, Action::Param),             // 2
        pack(State::CsiIntermediate, Action::Collect),    // 3
        pack(State::CsiIgnore, Action::None),             // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::CsiDispatch),         // 6
        pack(State::CsiIgnore, Action::None),             // 7
        pack(State::CsiIgnore, Action::None),             // 8
        pack(State::Ground, Action::CsiDispatch),         // 9: (Treat P as dispatch)
        0,0,0,0,0,0
    ],
    // State 4: CsiParam
    [
        pack(State::CsiParam, Action::Execute),           // 0
        pack(State::CsiIgnore, Action::None),             // 1
        pack(State::CsiParam, Action::Param),             // 2
        pack(State::CsiIntermediate, Action::Collect),    // 3
        pack(State::CsiIgnore, Action::None),             // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::CsiDispatch),         // 6
        pack(State::CsiIgnore, Action::None),             // 7
        pack(State::CsiIgnore, Action::None),             // 8
        pack(State::Ground, Action::CsiDispatch),         // 9: (Treat P as dispatch)
        0,0,0,0,0,0
    ],
    // State 5: CsiIntermediate
    [
        pack(State::CsiIntermediate, Action::Execute),    // 0
        pack(State::CsiIgnore, Action::None),             // 1
        pack(State::CsiIgnore, Action::Param),            // 2
        pack(State::CsiIntermediate, Action::Collect),    // 3
        pack(State::CsiIgnore, Action::None),             // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::CsiDispatch),         // 6
        pack(State::CsiIgnore, Action::None),             // 7
        pack(State::CsiIgnore, Action::None),             // 8
        pack(State::Ground, Action::CsiDispatch),         // 9: (Treat P as dispatch)
        0,0,0,0,0,0
    ],
    // State 6: CsiIgnore
    [
        pack(State::CsiIgnore, Action::Execute),          // 0
        pack(State::CsiIgnore, Action::Ignore),           // 1
        pack(State::CsiIgnore, Action::Ignore),           // 2
        pack(State::CsiIgnore, Action::Ignore),           // 3
        pack(State::CsiIgnore, Action::Ignore),           // 4
        pack(State::Escape, Action::Clear),               // 5
        pack(State::Ground, Action::Ignore),              // 6
        pack(State::CsiIgnore, Action::Ignore),           // 7
        pack(State::CsiIgnore, Action::Ignore),           // 8
        pack(State::Ground, Action::Ignore),              // 9
        0,0,0,0,0,0
    ],
    // State 7: DcsEntry
    [
        pack(State::DcsEntry, Action::Execute),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsParam, Action::Param),
        pack(State::DcsIntermediate, Action::Collect),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::Escape, Action::Clear),
        pack(State::DcsPassthrough, Action::Hook),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsPassthrough, Action::Hook),       // 9
        0,0,0,0,0,0
    ],
    // State 8: DcsParam
    [
        pack(State::DcsParam, Action::Execute),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsParam, Action::Param),
        pack(State::DcsIntermediate, Action::Collect),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::Escape, Action::Clear),
        pack(State::DcsPassthrough, Action::Hook),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsPassthrough, Action::Hook),       // 9
        0,0,0,0,0,0
    ],
    // State 9: DcsIntermediate
    [
        pack(State::DcsIntermediate, Action::Execute),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Param),
        pack(State::DcsIntermediate, Action::Collect),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::Escape, Action::Clear),
        pack(State::DcsPassthrough, Action::Hook),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsPassthrough, Action::Hook),       // 9
        0,0,0,0,0,0
    ],
    // State 10: DcsPassthrough (Wait for ST)
    [
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::Escape, Action::Unhook),            // <--- FIXED: Esc now triggers Unhook (emit), not Clear
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),
        pack(State::DcsPassthrough, Action::Put),       // 9
        0,0,0,0,0,0
    ],
    // State 11: DcsIgnore (Wait for ST)
    [
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::Escape, Action::Clear),
        pack(State::Ground, Action::Unhook),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::DcsIgnore, Action::Ignore),
        pack(State::Ground, Action::Unhook),            // 9
        0,0,0,0,0,0
    ],
    // State 12: OscString
    [
        pack(State::Ground, Action::OscEnd),        // 0: Exe (BEL)
        pack(State::OscString, Action::OscPut),     // 1: Print
        pack(State::OscString, Action::OscPut),     // 2: Param
        pack(State::OscString, Action::OscPut),     // 3: Inter
        pack(State::OscString, Action::OscPut),     // 4: Csi
        pack(State::Escape, Action::OscEnd),        // 5: Esc (ST)
        pack(State::OscString, Action::OscPut),     // 6: Disp
        pack(State::OscString, Action::OscPut),     // 7: Osc
        pack(State::OscString, Action::OscPut),     // 8: Sos
        pack(State::OscString, Action::OscPut),     // 9: DcsEntry (Put in OSC)
        0,0,0,0,0,0
    ],
    // State 13: SosPmApcString
    [
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::Escape, Action::Clear),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),
        pack(State::SosPmApcString, Action::Ignore),    // 9
        0,0,0,0,0,0
    ],
];