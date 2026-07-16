use std::path::Path;
use std::{fs::File, io::Write, os::windows::ffi::OsStrExt};

use anyhow::{Context, Result, anyhow};
use serde::{Serialize, de::DeserializeOwned};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

use crate::{models::AppSettings, paths::DataPaths};

pub fn load_settings(paths: &DataPaths) -> AppSettings {
    read_encrypted_json(&paths.settings_file)
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn save_settings(paths: &DataPaths, settings: &AppSettings) -> Result<()> {
    write_encrypted_json(&paths.settings_file, settings)
}

pub fn read_encrypted_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(json) = read_encrypted(path)? else {
        return Ok(None);
    };
    serde_json::from_str(&json)
        .with_context(|| format!("parse encrypted JSON from {}", path.display()))
        .map(Some)
}

pub fn write_encrypted_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    write_encrypted(path, &json)
}

pub fn read_encrypted(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let encrypted =
        std::fs::read(path).with_context(|| format!("read encrypted file {}", path.display()))?;
    let plain = unprotect(&encrypted)?;
    String::from_utf8(plain)
        .map(Some)
        .map_err(|error| anyhow!(error).context("encrypted data is not UTF-8"))
}

pub fn write_encrypted(path: &Path, value: &str) -> Result<()> {
    let encrypted = protect(value.as_bytes())?;
    let temporary = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create temporary encrypted file {}", temporary.display()))?;
        file.write_all(&encrypted)
            .with_context(|| format!("write temporary encrypted file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flush temporary encrypted file {}", temporary.display()))?;
    }
    let source = wide_path(&temporary);
    let destination = wide_path(path);
    let result = unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("commit encrypted file {}", path.display()));
    }
    Ok(())
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn protect(data: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).context("DPAPI input is too large")?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    copy_and_free_blob(output)
}

fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).context("DPAPI input is too large")?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    copy_and_free_blob(output)
}

fn copy_and_free_blob(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>> {
    if blob.cbData == 0 {
        if !blob.pbData.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
            }
        }
        return Ok(Vec::new());
    }
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(anyhow!("DPAPI returned an invalid data blob"));
    }
    let result = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec() };
    if !blob.pbData.is_null() {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip_uses_raw_ciphertext() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.dat");
        write_encrypted(&path, "{\"hello\":\"world\"}").unwrap();

        let encrypted = std::fs::read(&path).unwrap();
        assert!(!encrypted.starts_with(b"{"));
        assert_eq!(
            read_encrypted(&path).unwrap().unwrap(),
            "{\"hello\":\"world\"}"
        );

        write_encrypted(&path, "{\"hello\":\"rust\"}").unwrap();
        assert_eq!(
            read_encrypted(&path).unwrap().unwrap(),
            "{\"hello\":\"rust\"}"
        );
    }
}
