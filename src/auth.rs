//! パスワードのハッシュ (argon2id)

use anyhow::{anyhow, Result};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

pub fn hash(password: &str) -> Result<String> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow!("パスワードのハッシュに失敗: {e}"))?
        .to_string())
}

pub fn verify(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(h) => Argon2::default().verify_password(password.as_bytes(), &h).is_ok(),
        Err(_) => false,
    }
}

/// 計算に時間がかかるので tokio のブロッキングスレッドで実行する
pub async fn hash_async(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash(&password)).await?
}

pub async fn verify_async(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify(&password, &hash)).await.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let h = super::hash("ひみつ123").unwrap();
        assert!(super::verify("ひみつ123", &h));
        assert!(!super::verify("ひみつ124", &h));
        assert!(!super::verify("x", "!"));
    }
}
