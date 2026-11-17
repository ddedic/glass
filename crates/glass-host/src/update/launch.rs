//! Keep the installer alive across the host restart, with output in update.log.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub(super) fn spawn(mut cmd: Command, log: &Path) -> io::Result<Child> {
    cmd.stdin(Stdio::null())
        .stdout(File::create(log)?)
        .stderr(File::options().append(true).open(log)?);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Detached Windows PowerShell can exit successfully without running -Command.
        // Hide its console instead; retain the group and job breakaway for host restarts.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "glass-update-launch-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait(child: &mut Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("installer did not exit within 20 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "requires a Windows session that permits job breakaway"
    )]
    fn installer_executes_and_logs_output() {
        let scratch = Scratch::new();
        let log = scratch.0.join("update.log");
        let marker = scratch.0.join("executed");
        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
                .arg("[IO.File]::WriteAllText($env:GLASS_UPDATE_TEST_MARKER, 'executed'); Write-Output 'installer stdout'; [Console]::Error.WriteLine('installer stderr')");
            cmd
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "printf executed > \"$GLASS_UPDATE_TEST_MARKER\"; printf 'installer stdout\\n'; printf 'installer stderr\\n' >&2"]);
            cmd
        };
        cmd.env("GLASS_UPDATE_TEST_MARKER", &marker);
        let mut child = spawn(cmd, &log).unwrap();
        assert!(wait(&mut child).success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "executed");
        let output = std::fs::read_to_string(&log).unwrap();
        assert!(output.contains("installer stdout"), "{output}");
        assert!(output.contains("installer stderr"), "{output}");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a Windows session that permits job breakaway"]
    fn installer_survives_launcher_exit() {
        let scratch = Scratch::new();
        let fixture = concat!(module_path!(), "::installer_parent_fixture");
        // The test runner filter excludes the crate name.
        let fixture = fixture.split_once("::").unwrap().1;
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", fixture])
            .env("GLASS_UPDATE_TEST_DIR", &scratch.0)
            .spawn()
            .unwrap();
        assert!(wait(&mut parent).success());
        let marker = scratch.0.join("survived");
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::fs::read_to_string(&marker).ok().as_deref() != Some("survived") {
            assert!(
                Instant::now() < deadline,
                "installer did not finish after its launcher exited"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess fixture for installer_survives_launcher_exit"]
    fn installer_parent_fixture() {
        let Some(dir) = std::env::var_os("GLASS_UPDATE_TEST_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg("while (Get-Process -Id $env:GLASS_UPDATE_TEST_PARENT -ErrorAction SilentlyContinue) { Start-Sleep -Milliseconds 20 }; [IO.File]::WriteAllText($env:GLASS_UPDATE_TEST_MARKER, 'survived')")
            .env("GLASS_UPDATE_TEST_PARENT", std::process::id().to_string())
            .env("GLASS_UPDATE_TEST_MARKER", dir.join("survived"));
        // Dropping a Child does not terminate it; exiting the launcher must not either.
        drop(spawn(cmd, &dir.join("update.log")).unwrap());
    }
}
