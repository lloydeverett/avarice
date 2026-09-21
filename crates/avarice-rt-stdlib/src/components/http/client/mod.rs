// Derived from Astra <https://github.com/ArkForgeLabs/Astra>, src/components/http/client/mod.rs
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0.
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
