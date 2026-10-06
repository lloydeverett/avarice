// Derived from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/mod.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - Removed `pub mod database;`, `pub mod import;` and `pub mod templates;`: those components are
//     not taken.
//   - Removed `register_components`, which registered every component eagerly; this crate registers
//     each module lazily from `src/modules.rs`.
//   - Removed `read_from_stdlib` and its `#[allow(dead_code)]`, which read Astra's embedded
//     standard library through `crate::ASTRA_STD_LIBS` (defined in Astra's `main.rs`).
//   - Respelled `mlua::SerializeOptions` as `mlua::serde::SerializeOptions`: mlua 0.12, which this
//     workspace is on, moved it; Astra is on 0.11.
//   - Added a `__tostring` metamethod to the userdata `astra_buffer_types!` defines, `AstraBuffer`
//     and `AstraBufferMut`. Astra has none, so `tostring` and `print` give a userdata's type name
//     and its address. Now `tostring` gives `AstraBuffer(len <bytes>)`, and likewise for
//     `AstraBufferMut`, or `(in use)` in place of the length while a `read` or `write` holds the
//     buffer. The contents are left out on purpose: a buffer can be any size. Nothing Astra does
//     is altered: this is an addition.
//   - Put each module declaration behind the Cargo feature that compiles it in (ADR 0007):
//     `crypto`, `datetime`, `file_system` and `http` behind `stdlib-crypto`, `stdlib-datetime`,
//     `stdlib-fs` and `stdlib-http`. `astra_serde` and `utils` are behind `_astra_serde` and
//     `_astra_utils` instead, which the modules that borrow from them (`http`, `validation`) turn
//     on without registering `serde` or `utils`. Added a comment saying so.
//   - Put the shared items behind `_astra_buffers`, which `stdlib-http`, `stdlib-fs` and
//     `stdlib-process` turn on:
//     the `use mlua::{ExternalError, FromLua, LuaSerdeExt}` line that only they need,
//     `astra_buffer_types!` and its two invocations `AstraBuffer` and `AstraBufferMut`, `macros`,
//     `is_table_json` and `is_table_byte_array`.
//   - Allowed `dead_code`, under `cfg_attr`, where a build with only some of the features leaves
//     part of a file unused: on `astra_serde` unless `stdlib-serde` is on (`http` alone), on `utils`
//     unless `stdlib-utils` is on (`validation` alone), on `is_table_json` unless `stdlib-http` is
//     on (`fs` alone), on `is_table_byte_array` unless `stdlib-http` or `stdlib-fs` is on
//     (`process` alone), and, inside `astra_buffer_types!`, on the struct and on `new` unless both
//     `stdlib-http` and `stdlib-fs` are on (`AstraBuffer` is `http`'s and `AstraBufferMut` is
//     `fs`'s). Added a comment saying so.
//   - Nothing Astra does is altered by any of these: they are `#[cfg]`, `#[cfg_attr]` and
//     `#[allow]` attributes, and in a build with every feature on the file is as it was, apart
//     from the buffer changes below.
//   - Added `pub mod dirs;`, behind `stdlib-dirs`, for a module that owes Astra nothing (ADR 0014).
//     Nothing Astra does is altered: this is an addition, like the module declarations above it.
//   - Added a comment above `pub mod datetime;` saying that the module it declares is no longer
//     Astra's (ADR 0019). The declaration itself is unchanged. Nothing Astra does is altered: this
//     is an addition.
//   - Added `pub mod process;`, behind `stdlib-process`, for another module that owes Astra nothing
//     (ADR 0016), and which uses `AstraBuffer` for the output it captures and takes either buffer
//     as input. `stdlib-process` turns on `_astra_buffers`. Nothing Astra does is altered: this is
//     an addition.
//   - Changed the userdata `astra_buffer_types!` defines so that bytes reach Lua exactly
//     (ADR 0015). This alters what Astra does:
//       - `bytes` returns a Lua string holding exactly the buffer's bytes, made with
//         `lua.create_string`, where Astra returned a `Vec<u8>`, which Lua gets as a table with one
//         number per byte: 16 bytes of memory per byte, and impossible to turn back into a string
//         past about a million entries.
//       - Removed `text`. With `bytes` giving a string, `text` could differ from it only by
//         replacing invalid UTF-8 with U+FFFD, which is the defect.
//       - `json` parses the bytes with `serde_json::from_slice`, which fails on invalid UTF-8,
//         where Astra parsed `String::from_utf8_lossy` of them, which replaced it. Its `null`
//         handling is unchanged.
//       - Added a `__len` metamethod giving the length in bytes, so that `#buffer` need not copy
//         the bytes into Lua. It waits for the buffer, as `bytes` does. This is an addition.
//   - Everything else is unchanged.

#[cfg(feature = "_astra_buffers")]
use mlua::{ExternalError, FromLua, LuaSerdeExt};

// `astra_serde` is compiled for `http` as well as for `serde`, and `utils` for `validation` as well
// as for `utils`; see the header.
#[cfg(feature = "_astra_serde")]
#[cfg_attr(not(feature = "stdlib-serde"), allow(dead_code))]
pub mod astra_serde;
#[cfg(feature = "stdlib-crypto")]
pub mod crypto;
// Original to avarice since ADR 0019, and no longer Astra's; see its own header.
#[cfg(feature = "stdlib-datetime")]
pub mod datetime;
// Original to avarice, not Astra's; see its own header and ADR 0014.
#[cfg(feature = "stdlib-dirs")]
pub mod dirs;
#[cfg(feature = "stdlib-fs")]
pub mod file_system;
#[cfg(feature = "stdlib-http")]
pub mod http;
#[cfg(feature = "stdlib-process")]
pub mod process;
#[cfg(feature = "_astra_utils")]
#[cfg_attr(not(feature = "stdlib-utils"), allow(dead_code))]
pub mod utils;

#[cfg(feature = "_astra_buffers")]
macro_rules! astra_buffer_types {
    ($name:ident, $buffer_type:ty) => {
        // `http` uses `AstraBuffer` and `fs` uses `AstraBufferMut`, so a build with one of the two
        // leaves the other unused.
        #[cfg_attr(
            not(all(feature = "stdlib-http", feature = "stdlib-fs")),
            allow(dead_code)
        )]
        #[derive(Debug, Clone, FromLua)]
        pub struct $name(std::sync::Arc<tokio::sync::Mutex<$buffer_type>>);
        macros::impl_deref!($name, std::sync::Arc<tokio::sync::Mutex<$buffer_type>>);
        impl $name {
            #[cfg_attr(
                not(all(feature = "stdlib-http", feature = "stdlib-fs")),
                allow(dead_code)
            )]
            pub fn new(bytes: $buffer_type) -> Self {
                Self(std::sync::Arc::new(tokio::sync::Mutex::new(bytes)))
            }
        }
        impl mlua::UserData for $name {
            fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
                methods.add_meta_method(mlua::MetaMethod::ToString, |_, this, ()| {
                    // `try_lock`, since printing is not a place to wait for whoever holds it.
                    Ok(match this.try_lock() {
                        Ok(bytes) => format!("{}(len {})", stringify!($name), bytes.len()),
                        Err(_) => format!("{}(in use)", stringify!($name)),
                    })
                });
                methods.add_async_method("bytes", |lua, this, ()| async move {
                    let bytes = this.lock().await;
                    lua.create_string(&bytes[..])
                });
                methods.add_async_method("json", |lua, this, ()| async move {
                    let bytes = this.lock().await;
                    match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(parsed_json) => lua.to_value_with(
                            &parsed_json,
                            mlua::serde::SerializeOptions::new()
                                .serialize_none_to_null(false)
                                .serialize_unit_to_null(false),
                        ),
                        Err(e) => Err(e.into_lua_err()),
                    }
                });
                methods.add_async_meta_method(mlua::MetaMethod::Len, |_, this, ()| async move {
                    Ok(this.lock().await.len())
                });
            }
        }
    };
}

#[cfg(feature = "_astra_buffers")]
astra_buffer_types!(AstraBuffer, bytes::Bytes);
#[cfg(feature = "_astra_buffers")]
astra_buffer_types!(AstraBufferMut, bytes::BytesMut);

#[cfg(feature = "_astra_buffers")]
#[allow(unused)]
pub mod macros {
    macro_rules! impl_deref {
        ($struct:ty,$type:ty) => {
            impl std::ops::Deref for $struct {
                type Target = $type;

                fn deref(&self) -> &Self::Target {
                    &self.0
                }
            }
            impl std::ops::DerefMut for $struct {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.0
                }
            }
        };
    }

    macro_rules! impl_deref_field {
        ($struct:ty,$type:ty,$field:ident) => {
            impl std::ops::Deref for $struct {
                type Target = $type;

                fn deref(&self) -> &Self::Target {
                    &self.$field
                }
            }
            impl std::ops::DerefMut for $struct {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.$field
                }
            }
        };
    }

    pub(crate) use impl_deref;
    pub(crate) use impl_deref_field;
}

#[cfg(feature = "_astra_buffers")]
#[cfg_attr(not(feature = "stdlib-http"), allow(dead_code))]
fn is_table_json(table: &mlua::Table) -> mlua::Result<bool> {
    let mut has_string_key = false;
    let mut has_non_sequential_integer_key = false;
    let mut max_int_key = 0;

    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (key, _) = pair?;
        match key {
            mlua::Value::String(_) => has_string_key = true,
            mlua::Value::Integer(i) => {
                if i <= 0 || i > max_int_key + 1 {
                    has_non_sequential_integer_key = true;
                }
                max_int_key = max_int_key.max(i);
            }
            _ => return Ok(true), // Other key types (e.g., floats, booleans) are JSON-like
        }
    }

    Ok(has_string_key || has_non_sequential_integer_key)
}

#[cfg(feature = "_astra_buffers")]
#[cfg_attr(
    not(any(feature = "stdlib-http", feature = "stdlib-fs")),
    allow(dead_code)
)]
pub(crate) fn is_table_byte_array(table: &mlua::Table) -> mlua::Result<bool> {
    let mut i = 1;
    for pair in table.pairs::<i64, i64>() {
        match pair {
            Ok((key, value)) => {
                if key != i || !(0..=255).contains(&value) {
                    return Ok(false);
                }
                i += 1;
            }
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}
