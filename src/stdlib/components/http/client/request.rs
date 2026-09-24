// Taken from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/http/client/request.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - Made a body given as a Lua string be sent as that string's exact bytes (ADR 0015), where
//     Astra sent `to_string_lossy()` of it, which replaced invalid UTF-8 with U+FFFD and so
//     corrupted binary data. `HTTPClientRequestBodyTypes::String` holds a `Vec<u8>` in place of a
//     `String`, and `body_parser` fills it with `value.as_bytes().to_vec()`. This alters what Astra
//     does. Its `text/plain` default content type is unchanged.
//   - Made `headers_parser` give each response header value as its exact bytes (ADR 0015), where
//     Astra gave `String::from_utf8_lossy` of them, which replaced bytes that are not UTF-8, such
//     as Latin-1 text, which HTTP allows in a value. It returns a `HashMap<String, mlua::BString>`
//     in place of a `HashMap<String, String>`, and its `map` and `collect` changed to match; its
//     signature, now longer than a line, is wrapped as `rustfmt` would. Header names are ASCII by
//     the protocol, and stay `String`. This alters what Astra does.
//   - Made a URL given to `http.request` as a bare string an error if it is not UTF-8, as it
//     already was in the table form, where Astra took `to_string_lossy()` of it and so quietly
//     requested a different URL (ADR 0015). The string form's `url` is
//     `lua.unpack(mlua::Value::String(..))`, the conversion the table form's `details.get("url")`
//     makes. This alters what Astra does.
//   - Made the path of a file to upload, given as a string or in a `{ name, path }` table, its
//     exact bytes on Unix, where a file name is bytes, and elsewhere an error if it is not valid
//     Unicode (ADR 0015). A file whose own name is not UTF-8 is an error on every platform: the
//     request carries the name as text, which reqwest can send only as UTF-8, and would otherwise
//     carry U+FFFD in place of what is not. Astra took `to_string_lossy()` of a string path, so a
//     file whose path was not UTF-8 was reported missing, and in a table, where `path` was read as
//     a `String`, such an entry was left out (see the next item). A new private function at the
//     end of the file, `upload_path`, makes the path and checks the name; the string branch calls
//     it in place of `PathBuf::from(&path.to_string_lossy())`, and `parse_table` reads `path` as
//     an `mlua::LuaString` and calls it, collecting a `Vec<(String, PathBuf)>` in place of a
//     `Vec<(String, String)>`. This alters what Astra does.
//   - Made a file to upload that cannot be read an error when the request is sent (ADR 0017),
//     where Astra sent the request without it. Astra read a table as one `{ name, path }` entry
//     and, if that failed, as a list, whose non-table values `pairs(..).flatten()` skipped and
//     whose failing entries `let _ = parse_table(..)` ignored; and its `_ => {}` arm ignored a
//     value that was neither a string nor a table. Now a table with a `name` or `path` key is one
//     entry, any other table a list, and every error propagates, with `ErrorContext::context`
//     (added to the `mlua` import) saying which field or entry was wrong; a comment says which
//     reading is which. `nil` still means no file, in a `mlua::Value::Nil` arm, and any other
//     value is an error, in a new `_` arm. This alters what Astra does.
//   - Everything else is unchanged.

use crate::components::{AstraBuffer, astra_serde::sanetize_lua_input};
use mlua::{ErrorContext, ExternalResult, LuaSerdeExt};
use reqwest::{Client, RequestBuilder};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum HTTPClientRequestBodyTypes {
    String(Vec<u8>),
    Json(serde_json::Value),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct HTTPClientRequest {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Option<HTTPClientRequestBodyTypes>,
    pub file: Option<mlua::Value>,
    pub form: HashMap<String, String>,
}

impl HTTPClientRequest {
    pub fn register_to_lua(lua: &mlua::Lua) -> mlua::Result<()> {
        let function = lua.create_function(|lua, details: mlua::Value| match details {
            mlua::Value::String(details) => Ok(Self {
                url: lua.unpack(mlua::Value::String(details))?,
                method: "GET".to_string(),
                headers: HashMap::new(),
                body: None,
                file: None,
                form: HashMap::new(),
            }),
            mlua::Value::Table(details) => {
                let mut headers: HashMap<String, String> =
                    details.get("headers").unwrap_or(HashMap::new());
                let body = details.get::<mlua::Value>("body")?;
                let body = Self::body_parser(lua, &mut headers, body)?;

                Ok(Self {
                    url: details.get("url")?,
                    method: details
                        .get::<String>("method")
                        .map(|method| method.to_uppercase())
                        .unwrap_or("GET".to_string()),
                    headers,
                    body,
                    file: details.get::<mlua::Value>("file").ok(),
                    form: details
                        .get::<HashMap<String, String>>("form")
                        .unwrap_or_default(),
                })
            }
            _ => Err(mlua::Error::runtime(
                "Bad argument, expected string or table",
            )),
        })?;
        lua.globals().set("astra_internal__http_request", function)
    }

    pub async fn request_builder(&self) -> mlua::Result<RequestBuilder> {
        let mut client = match self.method.to_uppercase().as_str() {
            "CONNECT" => Client::new().request(reqwest::Method::CONNECT, &self.url),
            "OPTIONS" => Client::new().request(reqwest::Method::OPTIONS, &self.url),
            "DELETE" => Client::new().request(reqwest::Method::DELETE, &self.url),
            "TRACE" => Client::new().request(reqwest::Method::TRACE, &self.url),
            "PATCH" => Client::new().request(reqwest::Method::PATCH, &self.url),
            "HEAD" => Client::new().request(reqwest::Method::HEAD, &self.url),
            "POST" => Client::new().request(reqwest::Method::POST, &self.url),
            "PUT" => Client::new().request(reqwest::Method::PUT, &self.url),
            "GET" => Client::new().request(reqwest::Method::GET, &self.url),
            _ => Client::new().request(
                reqwest::Method::from_bytes(self.method.to_uppercase().as_bytes())
                    .into_lua_err()?,
                &self.url,
            ),
        };

        if let Some(HTTPClientRequestBodyTypes::String(body)) = &self.body {
            client = client.body(body.clone())
        } else if let Some(HTTPClientRequestBodyTypes::Bytes(body)) = &self.body {
            client = client.body(body.clone())
        } else if let Some(HTTPClientRequestBodyTypes::Json(body)) = &self.body {
            client = client.json(&body)
        } else if let Some(file_field) = &self.file {
            let mut file_form = reqwest::multipart::Form::new();
            let mut files = Vec::new();

            fn parse_table(
                files: &mut Vec<(String, std::path::PathBuf)>,
                file_details: &mlua::Table,
            ) -> mlua::Result<()> {
                let filename = file_details
                    .get::<String>("name")
                    .context("a file to upload needs a string `name`")?;
                let path = upload_path(
                    &file_details
                        .get::<mlua::LuaString>("path")
                        .context("a file to upload needs a string `path`")?,
                )?;

                files.push((filename, path));

                Ok(())
            }

            match file_field {
                mlua::Value::String(path) => {
                    let path = upload_path(path)?;
                    let path_filename = path.clone();

                    let filename = path_filename
                        .file_name()
                        .and_then(|filename| filename.to_str())
                        .unwrap_or("file.txt")
                        .to_string();

                    file_form = file_form.file(filename, path).await?;
                }
                mlua::Value::Table(file_details) => {
                    // One `{ name, path }`, or a list of them.
                    if file_details.contains_key("name")? || file_details.contains_key("path")? {
                        parse_table(&mut files, file_details)?;
                    } else {
                        for pair in file_details.pairs::<mlua::Value, mlua::Table>() {
                            let (_, file_details) = pair.context(
                                "a list of files to upload holds something other than a table",
                            )?;
                            parse_table(&mut files, &file_details)?;
                        }
                    }

                    for (filename, path) in files {
                        file_form = file_form.file(filename, path).await?;
                    }
                }
                mlua::Value::Nil => {}
                _ => {
                    return Err(mlua::Error::runtime(
                        "a file to upload is a path, a `{ name, path }` table, or a list of them",
                    ));
                }
            }

            client = client.multipart(file_form)
        }

        if !self.headers.is_empty() {
            for (key, value) in self.headers.iter() {
                client = client.header(key, value);
            }
        }
        if !self.form.is_empty() {
            client = client.form(&self.form);
        }

        Ok(client)
    }

    pub fn body_parser(
        lua: &mlua::Lua,
        headers: &mut HashMap<String, String>,
        body: mlua::Value,
    ) -> mlua::Result<Option<HTTPClientRequestBodyTypes>> {
        match body.clone() {
            mlua::Value::String(value) => {
                if !headers.contains_key("Content-Type") {
                    headers.insert("Content-Type".to_string(), "text/plain".to_string());
                }
                Ok(Some(HTTPClientRequestBodyTypes::String(
                    value.as_bytes().to_vec(),
                )))
            }
            mlua::Value::Table(value) => {
                if crate::components::is_table_byte_array(&value)? {
                    return Ok(Some(HTTPClientRequestBodyTypes::Bytes(
                        lua.from_value::<Vec<u8>>(body.clone())?,
                    )));
                } else if crate::components::is_table_json(&value)? {
                    if !headers.contains_key("Content-Type") {
                        headers.insert("Content-Type".to_string(), "application/json".to_string());
                    }
                    return Ok(Some(HTTPClientRequestBodyTypes::Json(
                        lua.from_value::<serde_json::Value>(sanetize_lua_input(
                            lua,
                            body.clone(),
                        )?)?,
                    )));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub fn headers_parser(
        header_map: &reqwest::header::HeaderMap,
    ) -> HashMap<String, mlua::BString> {
        header_map
            .iter()
            .map(|(key, value)| (key.to_string(), value.as_bytes().into()))
            .collect::<std::collections::HashMap<String, mlua::BString>>()
    }

    pub async fn response_to_http_client_response(
        response: reqwest::Response,
    ) -> super::HTTPClientResponse {
        super::HTTPClientResponse {
            remote_address: response.remote_addr().map(|i| i.to_string()),
            headers: Self::headers_parser(response.headers()),
            status_code: response.status().as_u16(),
            url: response.url().to_string(),
            body: if let Ok(bytes) = response.bytes().await {
                AstraBuffer::new(bytes)
            } else {
                AstraBuffer::new(bytes::Bytes::new())
            },
        }
    }
}

/// A path to upload, from a Lua string. On Unix, where a file name is bytes, it is the string's
/// exact bytes; elsewhere it is the string if that is valid Unicode, and an error if not. Either
/// way the file's own name, which the request carries as text, must be UTF-8.
fn upload_path(path: &mlua::LuaString) -> mlua::Result<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path.as_bytes()));
        if path.file_name().is_some_and(|name| name.to_str().is_none()) {
            return Err(mlua::Error::runtime(format!(
                "the name of the file to upload at {} is not UTF-8, and is sent as text",
                path.display()
            )));
        }
        Ok(path)
    }
    #[cfg(not(unix))]
    {
        Ok(path
            .to_str()
            .context("the path of a file to upload is not valid Unicode")?
            .to_owned()
            .into())
    }
}
