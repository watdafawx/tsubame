# B12-D: self-update relaunch proven on a real install dir (rig)

Setup: debug builds with `--features update-test` (see "Testing a real relaunch" in `updater.md`), old exe
`0.2.1`, new `0.2.2` (signed with the DEV key, served by `tsubame-update-testserver` on rig loopback), per-user
style install dir `G:\rig-run\b12d\install` (no `portable.txt`), `LOCALAPPDATA`/`APPDATA` redirected to a scratch
dir. Code under test: main at `a1d27d6` (includes B12-A). Process lists sampled every 100 ms on rig
(pid | start time | window title | args). Click = the "Download and restart" / "Restart now" button.

| # | Scenario | Result | Evidence |
|---|---|---|---|
| 1 | single window, Download and restart | PASS | old pid 53408 gone, new pid 47828 (`--after-update 0`) with window 3.7 s after the click (download + verify + apply + restart) |
| 2a | second window open, update from the owner window | PASS | owner 49028 replaced by 47200 with window ~1.1 s; the second window (37644) untouched |
| 2b | second window open, update from the non-owner window | PASS | 46988 replaced by 54108 with window ~1.5 s; owner 36188 untouched, new copy did not forward to it |
| 3 | auto_install on: staged download, then Restart now | PASS | staged, exe still 0.2.1; click -> 47488 (old exe, `--after-update 0`, applies the stage) -> 56132 with window 1.8 s; installed 0.2.2, trial cleared |

After each run `state.json` says `installed_version 0.2.2`, `trial: null`, `previous 0.2.1`, and
`tsubame.exe.old` is kept. `tsubame.exe --version` prints `Tsubame 0.2.2`. (The exe's Windows FileVersion resource
still reads 0.2.0.0 because it comes from Cargo.toml; the running version comes from `TSUBAME_VERSION`.)

Raw samples:

```
== log1
06:22:08.657 +0.24s  53408|06:21:52.398|win=Tsubame|  
06:22:08.969 +0.55s  53408|06:21:52.398|win=|  
06:22:12.288 +3.87s  47828|06:22:11.916|win=Tsubame|"" --after-update 0
== log2b
06:23:31.460 +0.33s  36188|06:23:09.462|win=Tsubame| ;; 46988|06:23:14.227|win=Tsubame|--new-window
06:23:34.556 +3.43s  36188|06:23:09.462|win=Tsubame| ;; 46988|06:23:14.227|win=|--new-window
06:23:34.951 +3.82s  36188|06:23:09.462|win=Tsubame| ;; 54108|06:23:34.537|win=Tsubame|--after-update 0
== log2a
06:24:23.466 +0.23s  37644|06:24:11.435|win=Tsubame|--new-window ;; 49028|06:24:06.684|win=Tsubame|
06:24:25.101 +1.87s  37644|06:24:11.435|win=Tsubame|--new-window ;; 49028|06:24:06.684|win=|
06:24:25.453 +2.22s  37644|06:24:11.435|win=Tsubame|--new-window ;; 47200|06:24:25.119|win=Tsubame|--after-update 0
06:24:48.873 +25.64s  37644|06:24:11.435|win=| ;; 47200|06:24:25.119|win=|--after-update 0
06:24:49.132 +25.90s  
== log3
06:25:08.175 +0.23s  52760|06:24:54.724|win=Tsubame|
06:25:09.123 +1.17s  47488|06:25:08.879|win=|--after-update 0
06:25:09.739 +1.79s  56132|06:25:09.502|win=Tsubame|--after-update 0
```
