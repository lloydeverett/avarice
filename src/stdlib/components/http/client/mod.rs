// Derived from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/http/client/mod.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - Removed `mod websocket;` and its `#[allow(unused_imports)] use websocket::*;`: the WebSocket
//     client is not taken.

mod request;
#[allow(unused_imports)]
pub use request::*;
mod userdata;
#[allow(unused_imports)]
use userdata::*;
