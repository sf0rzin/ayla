use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign_verify::{PublicKey, Signature};
use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::PathBuf,
    process::ExitCode,
};

const BUFFER_SIZE: usize = 64 * 1024;

fn decode_tauri_public_key(wrapped: &str) -> Result<PublicKey, String> {
    let decoded = STANDARD
        .decode(wrapped.as_bytes())
        .map_err(|_| "the Tauri updater public key is not strict standard Base64".to_string())?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| "the decoded Tauri updater public key is not UTF-8".to_string())?;

    let without_final_newline = decoded
        .strip_suffix("\r\n")
        .or_else(|| decoded.strip_suffix('\n'))
        .unwrap_or(&decoded);
    let normalized = without_final_newline.replace("\r\n", "\n");

    if normalized.contains('\r') {
        return Err("the decoded Tauri updater public key has invalid line endings".to_string());
    }

    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.len() != 2
        || !lines[0].starts_with("untrusted comment: minisign public key")
        || lines[1].is_empty()
    {
        return Err(
            "the decoded Tauri updater public key is not one complete Minisign public key"
                .to_string(),
        );
    }

    PublicKey::decode(&normalized)
        .map_err(|error| format!("the decoded Minisign public key is invalid: {error}"))
}

fn decode_tauri_signature(wrapped: &str) -> Result<Signature, String> {
    let decoded = STANDARD
        .decode(wrapped.as_bytes())
        .map_err(|_| "the Tauri updater signature is not strict standard Base64".to_string())?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| "the decoded Tauri updater signature is not UTF-8".to_string())?;

    Signature::decode(&decoded)
        .map_err(|error| format!("the decoded Minisign signature is invalid: {error}"))
}

fn verify_reader<R: Read>(
    public_key: &PublicKey,
    signature: &Signature,
    mut reader: R,
) -> Result<(), String> {
    let mut verifier = public_key
        .verify_stream(signature)
        .map_err(|error| format!("the signature cannot be verified in streaming mode: {error}"))?;
    let mut buffer = [0_u8; BUFFER_SIZE];

    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("the installer could not be read: {error}"))?;
        if count == 0 {
            break;
        }
        verifier.update(&buffer[..count]);
    }

    verifier
        .finalize()
        .map_err(|error| format!("the installer signature is invalid: {error}"))
}

fn next_argument(
    arguments: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<OsString, String> {
    arguments
        .next()
        .ok_or_else(|| format!("missing required {name} argument"))
}

fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let installer = PathBuf::from(next_argument(&mut arguments, "installer")?);
    let signature_path = PathBuf::from(next_argument(&mut arguments, "signature")?);
    let wrapped_public_key = next_argument(&mut arguments, "Tauri public key")?
        .into_string()
        .map_err(|_| "the Tauri public key argument is not UTF-8".to_string())?;

    if arguments.next().is_some() {
        return Err("unexpected extra arguments".to_string());
    }

    let public_key = decode_tauri_public_key(&wrapped_public_key)?;
    let wrapped_signature = fs::read_to_string(&signature_path)
        .map_err(|error| format!("the detached signature could not be read: {error}"))?;
    let signature = decode_tauri_signature(&wrapped_signature)?;
    let installer = File::open(&installer)
        .map_err(|error| format!("the installer could not be opened: {error}"))?;

    verify_reader(&public_key, &signature, installer)?;
    println!("signature verified");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("update verification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    fn fixture() -> (PublicKey, Signature) {
        let wrapped_key = STANDARD.encode(PUBLIC_KEY.as_bytes());
        let wrapped_signature = STANDARD.encode(SIGNATURE.as_bytes());
        let public_key = decode_tauri_public_key(&wrapped_key).expect("public key should decode");
        let signature =
            decode_tauri_signature(&wrapped_signature).expect("signature should decode");
        (public_key, signature)
    }

    #[test]
    fn verifies_prehashed_fixture() {
        let (public_key, signature) = fixture();
        verify_reader(&public_key, &signature, Cursor::new(b"test"))
            .expect("signature should verify");
    }

    #[test]
    fn rejects_tampered_fixture() {
        let (public_key, signature) = fixture();
        assert!(verify_reader(&public_key, &signature, Cursor::new(b"Test")).is_err());
    }

    #[test]
    fn rejects_extra_decoded_lines() {
        let wrapped = STANDARD.encode(format!("{PUBLIC_KEY}extra\n").as_bytes());
        assert!(decode_tauri_public_key(&wrapped).is_err());
    }

    #[test]
    fn rejects_unwrapped_minisign_signature() {
        assert!(decode_tauri_signature(SIGNATURE).is_err());
    }
}
