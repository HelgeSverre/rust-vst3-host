//! Released VST3 3.8 MIDI mapping ABI, insulated from the prerelease layout in vst3 0.3.
//! https://github.com/steinbergmedia/vst3_pluginterfaces/blob/master/vst/ivstmidimapping2.h
use crate::midi::{MidiChannel, MidiController, MidiControllerAssignment};
use vst3::{
    ComPtr,
    Steinberg::{kResultOk, tresult, Vst::*},
};

const MAX_ASSIGNMENTS: usize = 16_384;

// The released SDK packs the registered bit into bank's MSB. vst3 0.3 generated
// a prerelease four-byte struct with the SAME interface ID. Never pass that struct.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Controller {
    byte1: u8,
    byte2: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Assignment {
    parameter_id: u32,
    bus: i32,
    channel: u8,
    controller: Controller,
}

impl Controller {
    fn from_public(controller: MidiController) -> Option<Self> {
        match controller {
            MidiController::Registered { bank, index } => Some(Self {
                byte1: bank | 0x80,
                byte2: index,
            }),
            MidiController::Assignable { bank, index } => Some(Self {
                byte1: bank,
                byte2: index,
            }),
            MidiController::Midi1(_) => None,
        }
    }
    fn public(self) -> MidiController {
        if self.byte1 & 0x80 != 0 {
            MidiController::Registered {
                bank: self.byte1 & 0x7f,
                index: self.byte2 & 0x7f,
            }
        } else {
            MidiController::Assignable {
                bank: self.byte1 & 0x7f,
                index: self.byte2 & 0x7f,
            }
        }
    }
}

/// Called only on the controller thread. None means the optional interface is absent
/// or returned an invalid/failed assignment list; the caller can use the legacy map.
pub(super) fn assignments(
    controller: &ComPtr<IEditController>,
    direction: BusDirections,
) -> Option<Vec<MidiControllerAssignment>> {
    let mapping = controller.cast::<IMidiMapping2>()?;
    // SAFETY: valid COM pointer; buffers are initialized, bounded, and alive for the call.
    // The list ABI is identical (count, pointer). Only its pointed-to element differs;
    // the cast compensates for the dependency's stale element declaration.
    unsafe {
        let n1 = mapping.getNumMidi1ControllerAssignments(direction) as usize;
        let n2 = mapping.getNumMidi2ControllerAssignments(direction) as usize;
        if n1.checked_add(n2)? > MAX_ASSIGNMENTS {
            return None;
        }
        let mut one = vec![std::mem::zeroed::<Midi1ControllerParamIDAssignment>(); n1];
        let mut two = vec![Assignment::default(); n2];
        let list1 = Midi1ControllerParamIDAssignmentList {
            count: n1 as u32,
            map: one.as_mut_ptr(),
        };
        let list2 = Midi2ControllerParamIDAssignmentList {
            count: n2 as u32,
            map: two.as_mut_ptr().cast(),
        };
        if (n1 > 0 && mapping.getMidi1ControllerAssignments(direction, &list1) != kResultOk)
            || (n2 > 0 && mapping.getMidi2ControllerAssignments(direction, &list2) != kResultOk)
        {
            return None;
        }
        let mut out = Vec::with_capacity(n1 + n2);
        for entry in one {
            let Some(channel) = MidiChannel::from_index(entry.channel) else {
                continue;
            };
            let controller = MidiController::Midi1(entry.controller as u16);
            if entry.busIndex >= 0 && controller.is_valid() {
                out.push(MidiControllerAssignment {
                    bus: entry.busIndex,
                    channel,
                    controller,
                    parameter_id: entry.pId,
                });
            }
        }
        for entry in two {
            let Some(channel) = MidiChannel::from_index(entry.channel) else {
                continue;
            };
            if entry.bus >= 0 {
                out.push(MidiControllerAssignment {
                    bus: entry.bus,
                    channel,
                    controller: entry.controller.public(),
                    parameter_id: entry.parameter_id,
                });
            }
        }
        Some(out)
    }
}

pub(super) fn learn(
    controller: &ComPtr<IEditController>,
    bus: i32,
    channel: MidiChannel,
    address: MidiController,
) -> bool {
    // SAFETY: controller pointer is live and this function is called on its UI thread.
    unsafe {
        if let Some(learn) = controller.cast::<IMidiLearn2>() {
            return match address {
                MidiController::Midi1(cc) => {
                    learn.onLiveMidi1ControllerInput(bus, channel.as_index(), cc as i16)
                        == kResultOk
                }
                _ => {
                    let Some(address) = Controller::from_public(address) else {
                        return false;
                    };
                    learn_midi2(learn.as_ptr(), bus, channel.as_index(), address) == kResultOk
                }
            };
        }
        match (address, controller.cast::<IMidiLearn>()) {
            (MidiController::Midi1(cc), Some(learn)) => {
                learn.onLiveMIDIControllerInput(bus, channel.as_index() as i16, cc as i16)
                    == kResultOk
            }
            _ => false,
        }
    }
}

// SAFETY: caller must provide a live released-SDK IMidiLearn2 pointer on the UI thread.
unsafe fn learn_midi2(
    ptr: *mut IMidiLearn2,
    bus: i32,
    channel: u8,
    address: Controller,
) -> tresult {
    // Correct the by-value parameter ABI to match the released SDK.
    let call: unsafe extern "system" fn(*mut IMidiLearn2, i32, u8, Controller) -> tresult =
        std::mem::transmute((*(*ptr).vtbl).onLiveMidi2ControllerInput);
    call(ptr, bus, channel, address)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn released_learn2_by_value_call_preserves_bus_channel_and_address() {
        use vst3::Steinberg::{kNoInterface, FUnknown, FUnknownVtbl, TUID};
        unsafe extern "system" fn query(
            _: *mut FUnknown,
            _: *const TUID,
            out: *mut *mut std::ffi::c_void,
        ) -> tresult {
            *out = std::ptr::null_mut();
            kNoInterface
        }
        unsafe extern "system" fn retain(_: *mut FUnknown) -> u32 {
            1
        }
        unsafe extern "system" fn midi1(_: *mut IMidiLearn2, _: i32, _: u8, _: i16) -> tresult {
            kNoInterface
        }
        unsafe extern "system" fn midi2(
            _: *mut IMidiLearn2,
            bus: i32,
            channel: u8,
            address: Controller,
        ) -> tresult {
            if (bus, channel, address.byte1, address.byte2) == (3, 15, 0x91, 63) {
                kResultOk
            } else {
                kNoInterface
            }
        }
        // Model the released native vtable independently of the dependency's signature.
        #[repr(C)]
        struct ReleasedVtbl {
            base: FUnknownVtbl,
            midi2: unsafe extern "system" fn(*mut IMidiLearn2, i32, u8, Controller) -> tresult,
            midi1: unsafe extern "system" fn(*mut IMidiLearn2, i32, u8, i16) -> tresult,
        }
        let vtbl = ReleasedVtbl {
            base: FUnknownVtbl {
                queryInterface: query,
                addRef: retain,
                release: retain,
            },
            midi2,
            midi1,
        };
        let mut interface = IMidiLearn2 {
            vtbl: (&vtbl as *const ReleasedVtbl).cast(),
        };
        // SAFETY: both interface and its correctly laid-out vtable remain live for this call.
        assert_eq!(
            unsafe {
                learn_midi2(
                    &mut interface,
                    3,
                    15,
                    Controller {
                        byte1: 0x91,
                        byte2: 63,
                    },
                )
            },
            kResultOk
        );
    }

    #[test]
    fn released_midi2_abi_layout_and_encoding() {
        assert_eq!(std::mem::size_of::<Controller>(), 2);
        assert_eq!(std::mem::size_of::<Assignment>(), 12);
        assert_eq!(std::mem::offset_of!(Assignment, controller), 9);
        let address = MidiController::Registered {
            bank: 17,
            index: 63,
        };
        let raw = Controller::from_public(address).unwrap();
        assert_eq!((raw.byte1, raw.byte2), (0x91, 63));
        assert_eq!(raw.public(), address);
    }
}
