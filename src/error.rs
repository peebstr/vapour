use std::path::PathBuf;
use thiserror::Error;

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_RUNTIME_ERR: i32 = 1;
pub const EXIT_USAGE_ERR: i32 = 2;
pub const EXIT_STORAGE_ERR: i32 = 3;
pub const EXIT_IPC_ERR: i32 = 4;

/// Strongly-typed domain error enumeration for all Vapour subsystems.
#[derive(Error, Debug)]
pub enum VapourError {
    #[error("POSIX IPC / IO Error: {0}")]
    Ipc(#[from] std::io::Error),

    #[error("TOML Configuration Parse Error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("Configuration Error: {0}")]
    Config(String),

    #[error("Binary VDF parse failure at offset {offset}: {reason}")]
    VdfCorruption { offset: usize, reason: String },

    #[error("VDF Parse Error: {0}")]
    VdfParse(String),

    #[error("Cgroup controller '{0}' inaccessible; ensure systemd unified hierarchy")]
    CgroupUnavailable(PathBuf),

    #[error("Game '{game}' requires unmounted media at '{mount_point}'")]
    DriveUnmounted { game: String, mount_point: PathBuf },

    #[error("Storage Error: {0}")]
    Storage(String),

    #[error("Game Launch Error: {0}")]
    Launch(String),

    #[error("Game Not Found: {0}")]
    GameNotFound(String),

    #[error("Steam Client Daemon Error: {0}")]
    SteamDaemon(String),

    #[error("CLI Usage Error: {0}")]
    Usage(String),

    #[error("Supervisor Daemon Error: {0}")]
    Daemon(String),

    #[error("Runtime Error: {0}")]
    Runtime(String),
}

impl VapourError {
    /// Returns the standardized POSIX process exit code for this error category.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => EXIT_USAGE_ERR,
            Self::DriveUnmounted { .. } | Self::Storage(_) => EXIT_STORAGE_ERR,
            Self::SteamDaemon(_) | Self::Ipc(_) => EXIT_IPC_ERR,
            _ => EXIT_RUNTIME_ERR,
        }
    }
}

pub type VapourResult<T> = Result<T, VapourError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_exit_codes() {
        assert_eq!(
            VapourError::Usage("Missing argument".into()).exit_code(),
            EXIT_USAGE_ERR
        );
        assert_eq!(
            VapourError::DriveUnmounted {
                game: "CS2".into(),
                mount_point: PathBuf::from("/mnt/games"),
            }
            .exit_code(),
            EXIT_STORAGE_ERR
        );
        assert_eq!(
            VapourError::Storage("udisksctl failed".into()).exit_code(),
            EXIT_STORAGE_ERR
        );
        assert_eq!(
            VapourError::SteamDaemon("Pipe missing".into()).exit_code(),
            EXIT_IPC_ERR
        );
        assert_eq!(
            VapourError::Runtime("Generic fail".into()).exit_code(),
            EXIT_RUNTIME_ERR
        );
        assert_eq!(
            VapourError::Launch("Execution failed".into()).exit_code(),
            EXIT_RUNTIME_ERR
        );
        assert_eq!(
            VapourError::GameNotFound("Unknown".into()).exit_code(),
            EXIT_RUNTIME_ERR
        );
        assert_eq!(
            VapourError::CgroupUnavailable(PathBuf::from("/sys/fs/cgroup")).exit_code(),
            EXIT_RUNTIME_ERR
        );
    }

    #[test]
    fn test_error_formatting() {
        let err = VapourError::VdfCorruption {
            offset: 42,
            reason: "Unexpected byte tag 0x09".into(),
        };
        assert_eq!(
            err.to_string(),
            "Binary VDF parse failure at offset 42: Unexpected byte tag 0x09"
        );
    }
}
