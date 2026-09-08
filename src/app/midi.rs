#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum MidiEvent<'a> {
    // -------- Channel Voice Messages --------
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    PolyAftertouch {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelAftertouch {
        channel: u8,
        pressure: u8,
    },
    PitchBend {
        channel: u8,
        value: i16,
    }, // -8192..8191

    // -------- System Common --------
    TimeCodeQuarterFrame(u8),
    SongPosition(u16), // 14-bit
    SongSelect(u8),
    TuneRequest,

    // -------- System Exclusive --------
    SysEx(&'a [u8]),

    // -------- System Real-Time --------
    TimingClock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    Reset,

    Unknown,
}
pub fn parse_midi<'a>(message: &'a [u8]) -> MidiEvent<'a> {
    if message.is_empty() {
        return MidiEvent::Unknown;
    }

    let status = message[0];

    // -------- System Real-Time (single byte, can appear anytime) --------
    match status {
        0xF8 => return MidiEvent::TimingClock,
        0xFA => return MidiEvent::Start,
        0xFB => return MidiEvent::Continue,
        0xFC => return MidiEvent::Stop,
        0xFE => return MidiEvent::ActiveSensing,
        0xFF => return MidiEvent::Reset,
        _ => {}
    }

    // -------- System Exclusive --------
    if status == 0xF0 {
        return MidiEvent::SysEx(message);
    }

    // -------- System Common --------
    match status {
        0xF1 if message.len() >= 2 => return MidiEvent::TimeCodeQuarterFrame(message[1]),

        0xF2 if message.len() >= 3 => {
            let value = ((message[2] as u16) << 7) | message[1] as u16;
            return MidiEvent::SongPosition(value);
        }

        0xF3 if message.len() >= 2 => return MidiEvent::SongSelect(message[1]),

        0xF6 => return MidiEvent::TuneRequest,

        _ => {}
    }

    // -------- Channel Voice Messages --------
    if message.len() < 2 {
        return MidiEvent::Unknown;
    }

    let message_type = status & 0xF0;
    let channel = status & 0x0F;

    match message_type {
        0x80 if message.len() >= 3 => MidiEvent::NoteOff {
            channel,
            note: message[1],
            velocity: message[2],
        },

        0x90 if message.len() >= 3 => {
            let velocity = message[2];
            if velocity == 0 {
                MidiEvent::NoteOff {
                    channel,
                    note: message[1],
                    velocity: 0,
                }
            } else {
                MidiEvent::NoteOn {
                    channel,
                    note: message[1],
                    velocity,
                }
            }
        }

        0xA0 if message.len() >= 3 => MidiEvent::PolyAftertouch {
            channel,
            note: message[1],
            pressure: message[2],
        },

        0xB0 if message.len() >= 3 => MidiEvent::ControlChange {
            channel,
            controller: message[1],
            value: message[2],
        },

        0xC0 => MidiEvent::ProgramChange {
            channel,
            program: message[1],
        },

        0xD0 => MidiEvent::ChannelAftertouch {
            channel,
            pressure: message[1],
        },

        0xE0 if message.len() >= 3 => {
            let value = ((message[2] as i16) << 7) | message[1] as i16;
            MidiEvent::PitchBend {
                channel,
                value: value - 8192,
            }
        }

        _ => MidiEvent::Unknown,
    }
}
