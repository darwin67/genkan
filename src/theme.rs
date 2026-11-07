//! Legacy colors still read by the session lock's CPU renderer.
//!
//! The login greeter draws only from `crate::design`. The lock renderer moves
//! onto the design tokens in RFD 5 phase 3, after which this module goes away.

pub(crate) const AUTHENTICATION_INPUT_BORDER: [u8; 4] = [255, 255, 255, 120];
pub(crate) const AUTHENTICATION_INPUT_GLASS: [u8; 4] = [190, 205, 230, 54];
