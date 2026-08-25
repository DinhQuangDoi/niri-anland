//! In-memory input backend for the Anland compositor backend.
//!
//! Models the Anland daemon's input stream as a smithay [`InputBackend`] so
//! niri's shared input handling (`State::process_input_event`) can consume it.
//! Mirrors `smithay::backend::winit::input`: a marker type provides the
//! associated event types, all of which are constructed directly from the raw
//! Anland `InputEvent` payloads.

use std::path::PathBuf;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
    DeviceCapability, Event, InputBackend, KeyState, KeyboardKeyEvent, Keycode, PointerAxisEvent,
    PointerButtonEvent, PointerMotionAbsoluteEvent, TouchCancelEvent, TouchDownEvent, TouchEvent,
    TouchFrameEvent, TouchMotionEvent, TouchSlot, TouchUpEvent, UnusedEvent,
};
use smithay::output::Output;

use crate::input::backend_ext::NiriInputDevice;
use crate::niri::State;

/// Marker type identifying the Anland input backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnlandInput;

/// Single virtual device backing all Anland input events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnlandVirtualDevice;

impl Device for AnlandVirtualDevice {
    fn id(&self) -> String {
        "anland".to_owned()
    }

    fn name(&self) -> String {
        "anland virtual input".to_owned()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(
            capability,
            DeviceCapability::Keyboard | DeviceCapability::Pointer | DeviceCapability::Touch
        )
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<PathBuf> {
        None
    }
}

impl NiriInputDevice for AnlandVirtualDevice {
    fn output(&self, _state: &State) -> Option<Output> {
        None
    }
}

/// Convert a raw Anland screen-space coordinate into the given output size.
fn x_transformed(x: f64, screen_w: f64, width: i32) -> f64 {
    (x / screen_w * width as f64).clamp(0.0, width as f64)
}

fn y_transformed(y: f64, screen_h: f64, height: i32) -> f64 {
    (y / screen_h * height as f64).clamp(0.0, height as f64)
}

// ---------------------------------------------------------------------------
// Keyboard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct AnlandKeyboardEvent {
    pub time: u64,
    pub key_code: u32,
    pub state: KeyState,
}

impl Event<AnlandInput> for AnlandKeyboardEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl KeyboardKeyEvent<AnlandInput> for AnlandKeyboardEvent {
    fn key_code(&self) -> Keycode {
        // The daemon delivers Linux evdev codes; xkb keycodes are evdev + 8.
        Keycode::new(self.key_code + 8)
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        1
    }
}

// ---------------------------------------------------------------------------
// Touch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchDownEvent {
    pub time: u64,
    pub slot: u32,
    pub x: f64,
    pub y: f64,
    pub screen_w: f64,
    pub screen_h: f64,
}

impl Event<AnlandInput> for AnlandTouchDownEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl TouchEvent<AnlandInput> for AnlandTouchDownEvent {
    fn slot(&self) -> TouchSlot {
        Some(self.slot).into()
    }
}

impl AbsolutePositionEvent<AnlandInput> for AnlandTouchDownEvent {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        x_transformed(self.x, self.screen_w, width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        y_transformed(self.y, self.screen_h, height)
    }
}

impl TouchDownEvent<AnlandInput> for AnlandTouchDownEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchMotionEvent {
    pub time: u64,
    pub slot: u32,
    pub x: f64,
    pub y: f64,
    pub screen_w: f64,
    pub screen_h: f64,
}

impl Event<AnlandInput> for AnlandTouchMotionEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl TouchEvent<AnlandInput> for AnlandTouchMotionEvent {
    fn slot(&self) -> TouchSlot {
        Some(self.slot).into()
    }
}

impl AbsolutePositionEvent<AnlandInput> for AnlandTouchMotionEvent {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        x_transformed(self.x, self.screen_w, width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        y_transformed(self.y, self.screen_h, height)
    }
}

impl TouchMotionEvent<AnlandInput> for AnlandTouchMotionEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchUpEvent {
    pub time: u64,
    pub slot: u32,
}

impl Event<AnlandInput> for AnlandTouchUpEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl TouchEvent<AnlandInput> for AnlandTouchUpEvent {
    fn slot(&self) -> TouchSlot {
        Some(self.slot).into()
    }
}

impl TouchUpEvent<AnlandInput> for AnlandTouchUpEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchCancelEvent {
    pub time: u64,
    pub slot: u32,
}

impl Event<AnlandInput> for AnlandTouchCancelEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl TouchEvent<AnlandInput> for AnlandTouchCancelEvent {
    fn slot(&self) -> TouchSlot {
        Some(self.slot).into()
    }
}

impl TouchCancelEvent<AnlandInput> for AnlandTouchCancelEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchFrameEvent {
    pub time: u64,
}

impl Event<AnlandInput> for AnlandTouchFrameEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl TouchFrameEvent<AnlandInput> for AnlandTouchFrameEvent {}

// ---------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerMotionEvent {
    pub time: u64,
    pub x: f64,
    pub y: f64,
    pub screen_w: f64,
    pub screen_h: f64,
}

impl Event<AnlandInput> for AnlandPointerMotionEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl AbsolutePositionEvent<AnlandInput> for AnlandPointerMotionEvent {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        x_transformed(self.x, self.screen_w, width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        y_transformed(self.y, self.screen_h, height)
    }
}

impl PointerMotionAbsoluteEvent<AnlandInput> for AnlandPointerMotionEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerButtonEvent {
    pub time: u64,
    pub button_code: u32,
    pub state: ButtonState,
}

impl Event<AnlandInput> for AnlandPointerButtonEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl PointerButtonEvent<AnlandInput> for AnlandPointerButtonEvent {
    fn button_code(&self) -> u32 {
        self.button_code
    }

    fn state(&self) -> ButtonState {
        self.state
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerAxisEvent {
    pub time: u64,
    pub axis: Axis,
    pub value: f64,
    pub discrete: i32,
}

impl Event<AnlandInput> for AnlandPointerAxisEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> AnlandVirtualDevice {
        AnlandVirtualDevice
    }
}

impl PointerAxisEvent<AnlandInput> for AnlandPointerAxisEvent {
    fn source(&self) -> AxisSource {
        AxisSource::Wheel
    }

    fn amount(&self, axis: Axis) -> Option<f64> {
        (axis == self.axis).then_some(self.value)
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        (axis == self.axis).then_some(self.value * 120.0)
    }

    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

impl InputBackend for AnlandInput {
    type Device = AnlandVirtualDevice;
    type KeyboardKeyEvent = AnlandKeyboardEvent;
    type PointerAxisEvent = AnlandPointerAxisEvent;
    type PointerButtonEvent = AnlandPointerButtonEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = AnlandPointerMotionEvent;

    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;

    type TouchDownEvent = AnlandTouchDownEvent;
    type TouchUpEvent = AnlandTouchUpEvent;
    type TouchMotionEvent = AnlandTouchMotionEvent;
    type TouchCancelEvent = AnlandTouchCancelEvent;
    type TouchFrameEvent = AnlandTouchFrameEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;

    type SwitchToggleEvent = UnusedEvent;

    type SpecialEvent = UnusedEvent;
}
