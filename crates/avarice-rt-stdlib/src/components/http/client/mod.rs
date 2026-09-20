// Derived from Astra <https://github.com/ArkForgeLabs/Astra>, src/components/http/client/mod.rs
// Copyright (c) ArkForge Labs, licensed under the Apache License 2.0.
// See LICENSE and NOTICE in this crate's root.
//
// Changes from the original:
//   - Removed `mod websocket;` and its import: the WebSocket client is not taken.

mod request;
#[allow(unused_imports)]
pub use request::*;
mod userdata;
#[allow(unused_imports)]
use userdata::*;
