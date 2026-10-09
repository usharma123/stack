//! The Python the fake mise runs, found once on the test's own PATH: tests that narrow the
//! PATH stack sees (to leave out every other mise, say) must not change which Python the fake
//! gets, and its TOML reading needs `tomllib` (Python 3.11 or newer).

use std::path::PathBuf;
use std::sync::OnceLock;

/// An absolute path to a `python3` (or `python3.N`) on PATH that can import `tomllib`.
pub fn python() -> &'static PathBuf {
    static FOUND: OnceLock<PathBuf> = OnceLock::new();
    FOUND.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let names = ["python3", "python3.14", "python3.13", "python3.12", "python3.11"];
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| {
                candidate.is_file()
                    && std::process::Command::new(candidate)
                        .args(["-c", "import tomllib"])
                        .output()
                        .is_ok_and(|out| out.status.success())
            })
            .expect("the fake mise needs Python 3.11 or newer (with tomllib) on PATH")
    })
}
