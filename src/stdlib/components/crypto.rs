// Taken from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/crypto.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - Took every input, and gave the decoders' output, as `mlua::BString` in place of `String`, so
//     bytes pass through exactly (ADR 0015). Astra's `String` refused an input that was not UTF-8,
//     so `hash` and the encoders could not take binary data, and its decoders replaced invalid
//     UTF-8 in what they decoded with U+FFFD, so `decode("/wAB")` gave `EF BF BD 00 01` and not
//     `FF 00 01`. This alters what Astra does: `hash`, `base64_encode`, `base64_encode_urlsafe`,
//     `base64_decode` and `base64_decode_urlsafe` changed their parameter types, and the two
//     decoders pass `&input` to `decode_vec` in place of `input.as_bytes()` and return
//     `output_buf` itself rather than a lossy `String` of it. The five closures that register them
//     changed their parameter types to match, and the one for `hash`, now longer than a line, is
//     wrapped as `rustfmt` would.
//   - Everything else is unchanged.

// Cryptography util. Currently supporting only SHA2 and SHA3 (256 and 512 variants)

use base64::{
    Engine,
    prelude::{BASE64_STANDARD, BASE64_URL_SAFE},
};

pub fn register_to_lua(lua: &mlua::Lua) -> mlua::Result<()> {
    let hash_function = lua.create_function(|_, (hash_type, input): (String, mlua::BString)| {
        Ok(hash(hash_type, input))
    })?;
    lua.globals().set("astra_internal__hash", hash_function)?;

    lua.globals().set(
        "astra_internal__base64_encode",
        lua.create_function(|_, input: mlua::BString| Ok(base64_encode(input)))?,
    )?;

    lua.globals().set(
        "astra_internal__base64_encode_urlsafe",
        lua.create_function(|_, input: mlua::BString| Ok(base64_encode_urlsafe(input)))?,
    )?;

    lua.globals().set(
        "astra_internal__base64_decode",
        lua.create_function(|_, input: mlua::BString| base64_decode(input))?,
    )?;

    lua.globals().set(
        "astra_internal__base64_decode_urlsafe",
        lua.create_function(|_, input: mlua::BString| base64_decode_urlsafe(input))?,
    )?;

    Ok(())
}

fn hash(hash_type: String, input: mlua::BString) -> String {
    macro_rules! sha_impl {
        ($hash_function:ty) => {
            let mut sha = <$hash_function>::new();
            sha.update(input);
            let result = sha.finalize();
            return result.iter().map(|b| format!("{:02x}", b)).collect()
        };
    }
    if hash_type.starts_with("sha") {
        match hash_type.as_str() {
            "sha3_256" => {
                use sha3::Digest;
                sha_impl!(sha3::Sha3_256);
            }
            "sha3_512" => {
                use sha3::Digest;
                sha_impl!(sha3::Sha3_512);
            }
            "sha2_512" => {
                use sha3::Digest;
                sha_impl!(sha2::Sha512);
            }
            _ => {
                use sha2::Digest;
                sha_impl!(sha2::Sha256);
            }
        }
    } else {
        "".to_string()
    }
}

fn base64_encode(input: mlua::BString) -> String {
    let mut output_buf = String::new();
    BASE64_STANDARD.encode_string(input, &mut output_buf);
    output_buf
}

fn base64_encode_urlsafe(input: mlua::BString) -> String {
    let mut output_buf = String::new();
    BASE64_URL_SAFE.encode_string(input, &mut output_buf);
    output_buf
}

fn base64_decode(input: mlua::BString) -> mlua::Result<mlua::BString> {
    let mut output_buf = Vec::new();
    match BASE64_STANDARD.decode_vec(&input, &mut output_buf) {
        Ok(_) => Ok(output_buf.into()),
        Err(e) => Err(mlua::Error::runtime(format!(
            "Could not decode the base64 encoded input: {e:?}"
        ))),
    }
}

fn base64_decode_urlsafe(input: mlua::BString) -> mlua::Result<mlua::BString> {
    let mut output_buf = Vec::new();
    match BASE64_URL_SAFE.decode_vec(&input, &mut output_buf) {
        Ok(_) => Ok(output_buf.into()),
        Err(e) => Err(mlua::Error::runtime(format!(
            "Could not decode the base64 encoded input: {e:?}"
        ))),
    }
}
