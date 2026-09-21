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
//   - Put each module declaration behind the Cargo feature that compiles it in (ADR 0007), and the
//     shared items below (`AstraBuffer` and `AstraBufferMut`, `macros`, `is_table_json` and
//     `is_table_byte_array`) behind `_astra_buffers`, which `stdlib-http` and `stdlib-fs` turn on.
//     `astra_serde` and `utils` are behind `_astra_serde` and `_astra_utils`, which the modules
//     that borrow from them (`http`, `validation`) turn on without registering `serde` or `utils`.
//     A build that has only some of these leaves part of a file unused (`astra_serde` under `http`
//     alone, `utils` under `validation` alone, and one of the two buffer types and `is_table_json`
//     under `fs` alone), so the `dead_code` lint is allowed there. Nothing Astra does is altered:
//     these are `#[cfg]`, `#[cfg_attr]` and `#[allow]` attributes.
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
#[cfg(feature = "stdlib-datetime")]
pub mod datetime;
#[cfg(feature = "stdlib-fs")]
pub mod file_system;
#[cfg(feature = "stdlib-http")]
pub mod http;
#[cfg(feature = "_astra_utils")]
#[cfg_attr(not(feature = "stdlib-utils"), allow(dead_code))]
pub mod utils;

#[cfg(feature = "_astra_buffers")]
macro_rules! astra_buffer_types {
    ($name:ident, $buffer_type:ty) => {
        // `http` uses `AstraBuffer` and `fs` uses `AstraBufferMut`, so a build with one of the two
        // leaves the other unused.
        #[allow(dead_code)]
        #[derive(Debug, Clone, FromLua)]
        pub struct $name(std::sync::Arc<tokio::sync::Mutex<$buffer_type>>);
        macros::impl_deref!($name, std::sync::Arc<tokio::sync::Mutex<$buffer_type>>);
        impl $name {
            #[allow(dead_code)]
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
                methods.add_async_method("bytes", |_, this, ()| async move {
                    let bytes = this.lock().await;
                    Ok(bytes.to_vec())
                });
                methods.add_async_method("text", |_, this, ()| async move {
                    let bytes = this.lock().await;
                    Ok(String::from_utf8_lossy(&bytes).to_string())
                });
                methods.add_async_method("json", |lua, this, ()| async move {
                    let bytes = this.lock().await;
                    match serde_json::from_str::<serde_json::Value>(
                        &String::from_utf8_lossy(&bytes).to_string(),
                    ) {
                        Ok(parsed_json) => lua.to_value_with(
                            &parsed_json,
                            mlua::serde::SerializeOptions::new()
                                .serialize_none_to_null(false)
                                .serialize_unit_to_null(false),
                        ),
                        Err(e) => Err(e.into_lua_err()),
                    }
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
