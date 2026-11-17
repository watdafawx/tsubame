# Replacing Explorer as the default folder handler: Defender, signing, installer

Research task R1, 2026-11-17. Written for `docs/PLAN.md` §3 and §7 "P6. Ship" (installer, auto-updater,
"set as default file manager"). No code, no registry changes, no installers were run for this document.

**Web search and fetch were available for this task.** Every claim below is tagged:

- **[sourced]** — a URL in §8 says it, and I read it in this session (or the search result carried the
  page text verbatim).
- **[inferred]** — my reasoning from sourced facts, or from the absence of reports. Treat as a hypothesis
  to test on the VM, not as fact.

Where sources are thin or old I say so in §7. Detection names are quoted only where a source names them;
nothing here is invented.

---

## 0. Summary and recommendation

1. **Do not redirect `explorer.exe` through Image File Execution Options (IFEO).** Microsoft Defender has a
   live behaviour signature for it, `Behavior:Win32/ifeoDebugger.DB`, that fires on writing an IFEO
   `Debugger` value and removes the key; PSAppDeployToolkit deleted a long-standing feature over exactly
   this in 2025. IFEO is also MITRE T1546.012 and its "Mitigations" field says it cannot be easily
   mitigated. [sourced] This is the single highest-risk mechanism on the list and it gains us only Win+E.

2. **Use the per-user class store only**: `HKCU\Software\Classes\Directory\shell` with a default verb
   pointing at a **small signed handler shim** in the install directory. No HKLM, no HKCR, no
   `DelegateExecute` we own, no `CLSID\...\InprocServer32`, no DLL, no explorer patching. OneCommander does
   exactly this; Files, Directory Opus and Total Commander write the same kind of verb one class higher, on
   `Folder`. It is the only mechanism that has shipped in consumer file managers for years without a
   documented Defender detection [inferred from §2.5 — absence of reports, not a guarantee].

3. **Keep `Folder\shell` (the generic, virtual-folder class) out of the first version.** That key,
   `HKCU\Software\Classes\Folder\shell\open\command`, is the exact path Defender has a UAC-bypass signature
   for (`Behavior:Win32/UACBypassExp.F!sdclt`) and the exact path Microsoft's July 2026 threat report names
   as one of ChocoShell's three silent UAC bypasses. [sourced] We are not a UAC bypass — we write a plain
   command line, never `DelegateExecute` pointing at our own CLSID, never an elevation trigger — but there
   is no reason to stand on that particular square when `Directory\shell` covers double-click, the Run
   dialog and most "open folder" callers. OneCommander made the same call for the same reason. [sourced]

4. **Sign everything, but do not expect signing to stop SmartScreen or ML false positives.** Since 2024 EV
   certificates no longer bypass SmartScreen; OV and Microsoft Artifact Signing (ex-Trusted Signing) both
   start at "unrecognized, user must click Run anyway". A signed Inno Setup installer was still flagged
   `Trojan:Win32/Wacatac.B!ml` in Feb 2025. [sourced] Sign for *publisher identity and enterprise trust*,
   budget 2–8 weeks for reputation, and plan for one or two false-positive submissions per year.

5. **Per-user, no-admin install; portable zip first, Inno Setup second, MSIX never for this feature.**
   MSIX cannot write the registry keys we need from inside the package — the MSIX registry is a per-package
   overlay — which is why Files ships a separate unpackaged `Files.App.Launcher.exe` and why OneCommander
   *removed* "set as default" from its Store build. [sourced]

6. **The registered command must be a shim that fails safe.** Point the verb at
   `<install>\tsubame-handler.exe`, not at `tsubame.exe`. The shim forwards to a running instance or starts
   the app, and on any failure (app missing, app exited non-zero, no window within a timeout) it (a) removes
   its own registration and (b) launches `explorer.exe` with the path. Then the worst case for a user who
   deletes the install folder manually is one odd folder open, not a dead shell.

7. **Never touch defaults except in response to a click**, never re-assert them after an update, and never
   from the installer. Microsoft's unwanted-software criteria list "circumvent user consent dialogs",
   "install … without your permission" and "unauthorized registry changes" as reasons to classify software
   as unwanted or tampering. Re-asserting on every launch is how legitimate tools get classified.
   [sourced for the criteria, inferred for the mapping to our feature]

---

## 1. How existing file managers actually register (Q1)

### 1.1 The mechanics

There is **no supported Windows API for "default file manager"**. Programmatic default-setting for file
*extensions* was deprecated in Windows 8 (`IApplicationAssociationRegistration::SetAppAsDefault*`), and
Microsoft's own Default Programs documentation says "attempts by an application to modify default
association behaviors at a machine level post-installation will be unsuccessful" and that defaults must be
registered per-user and be user-driven. The supported path for extensions is only to *launch* the Settings
UI (`IApplicationAssociationRegistrationUI`). [sourced: default-programs] The `Folder` / `Directory` /
`Drive` shell-verb classes are **not** covered by that rule and have no `UserChoice` hash, which is the gap
every third-party file manager uses. [inferred, but consistent with every implementation in §1.2 and with
no vendor documenting an alternative]

Terminology that matters:

| Term | What it does |
|---|---|
| `HKCR\Directory\shell\(Default)` | Names the verb run when a *filesystem* folder is opened with the `open` verb. This is the double-click / `ShellExecute(path, "open")` path. |
| `HKCR\Folder\shell\open` | The *generic* folder class (filesystem folders plus virtual folders: Control Panel, Recycle Bin, This PC …). Stock value carries `DelegateExecute = {11dbb47c-a525-400b-9e80-a54615a090c0}` (CLSID_ExecuteFolder, `explorerframe.dll`). |
| `DelegateExecute` | Per-verb value naming a COM CLSID. **When present the shell uses it exclusively and does not fall back to the verb's command line.** [sourced: the OneMore registry script comment states exactly this; corroborated by the Total Commander write-up in §1.2] That is why changing `Folder\shell\open\command` alone does not work on modern Windows, and why the "fix" everyone documents is removing `DelegateExecute`. |
| `CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}` | The shell folder behind Win+E / "This PC"; its `shell\opennewwindow` verb is what the Win+E keystroke ends up invoking. [sourced: strontic CLSID page, and it is the key Files, OneCommander, Q-Dir and KDE users all write] |
| `HKCR` vs `HKCU\Software\Classes` | HKCR is a merge view. Writing to HKCR as a non-elevated app lands in HKCU; writing elevated lands in HKLM and affects every user. |

### 1.2 What the shipping apps do

| App | Mechanism (as documented/observed) | Admin | Notes |
|---|---|---|---|
| **Files** (files-community) | `HKCU\Software\Classes\Folder\shell\open\command` and `…\explore\command` = `"%LOCALAPPDATA%\Files\Files.App.Launcher.exe" "%1"`, plus `HKCU\Software\Classes\CLSID\{52205fd8-…}\shell\opennewwindow\command` = the launcher. Called "experimental". [sourced: issue #16111] | No | On the install observed in #16111 the `Folder` verbs still carried the stock `DelegateExecute = {11dbb47c-…}`, and a maintainer says setting it empty also works — i.e. Files' exact behaviour here is muddled; do not copy it. Packaged (MSIX) app, so it needs the separate **unpackaged launcher exe** to write the keys at all (§4). Their docs also offer a `Directory\shell` → `files.exe -Directory %1` recipe using the app-execution alias. [sourced: replace-file-explorer.md] |
| **OneCommander** | `HKCU\SOFTWARE\Classes\Directory\shell` (+ a `CLSID\{52205fd8-…}` key for Win+E), deliberately **not** `Folder\shell` "because Folder includes items that are not filesystem items". Ships `RestoreExplorerAsTheDefaultFileManager.reg`. [sourced: OC Google Group threads, onecommander.com/help2/Advanced.html] | No | The author's stated reasons for removing it from the Store build: users uninstalling without unchecking left Explorer unusable, and Store updates broke it. Uninstall page says: if you set it, **you must uncheck it first**. [sourced] |
| **Directory Opus** | "Explorer Replacement" writes shell-verb defaults; a 13.23 user on 24H2 reports `HKCR\Folder\shell` = `openindopus` and `Directory\shell` empty. Win+E is a separate setting. [sourced: docs.dopus.com manual + Opus forum thread June 2026] | Registry change; manual says USB/portable mode may fail "depending on your user permissions" → effectively machine-wide | Manual states Win+E is "intercepted by a different process to Explorer Replacement"; same 2026 thread shows Win+E failing on Win11 IoT LTSC 24H2 while double-click works. |
| **Total Commander** | Its installer can write the machine-wide variant: `HKCR\Drive\shell\open\command` and `HKCR\Directory\shell\open\command` = `TOTALCMD64.EXE /O "%1"`. [sourced: Opus forum post quoting the keys; ghisler wiki linked from it] | Yes (installer) | This is the best-documented **failure mode**: leftover keys from an uninstalled/removed TC installation produced `HKEY_CLASSES_ROOT\...` entries pointing at a missing exe, and the user got "Application not found" from Chrome's "show in folder" and from every desktop folder until the keys were deleted by hand. [sourced: same thread] |
| **Q-Dir** | Win+E via `HKCU\Software\Classes\CLSID\{52205fd8-…}\shell\opennewwindow\command` = the exe, `DelegateExecute = ""`. [sourced: superuser answer 241442, KDE forum post uses the identical recipe for Dolphin] | No | Portable-friendly. |
| **XYplorer** | Reported to write the `Directory` path only, not `Folder`; a user reports that this makes it fail for some callers (Bandizip). [sourced, but single user report in an OC thread — thin] | Unverified | — |
| **Explorer++** | Not verified in this session. I found no primary source describing what its "make Explorer++ the default" option writes — not even confirmation that the option still exists in current builds. **Treat as unknown; do not cite it in the implementation.** | — | — |
| **File Pilot** | No built-in option as of Apr 2025; a how-to guide tells users to hand-edit `HKCR\Drive\shell` and `HKCR\Directory\shell`. [sourced: howtogeek, secondary] | Yes (HKCR as written) | The OneCommander author replied to that article: "never use HKEY_CLASSES_ROOT for these kind of changes, always HKEY_CURRENT_USER". [sourced] |

### 1.3 Coverage: what each mechanism actually catches

| User action | `Directory\shell` default verb | `Folder\shell\open` override | Win+E CLSID key | IFEO on explorer.exe |
|---|---|---|---|---|
| Double-click a folder (Explorer, desktop) | Yes [sourced: OC + Opus users] | Yes | No | No (Explorer still opens) |
| `Win+R` → `C:\`, `start C:\x` in a terminal | Yes [sourced: TC write-up] | Yes | No | No |
| "Open folder" from a browser / app via ShellExecute | Yes [sourced: OC thread — Chrome/Whale opened OC] | Yes | No | No |
| Win+E | No | No | Yes, but unreliable on Win11 24H2+ [sourced: Opus 2026 thread] | No |
| Taskbar Explorer icon, Start menu → File Explorer | No — launches explorer.exe directly | No | No | No |
| "Show in folder" / "Open file location" (browser, apps) | No — these ask Explorer for a window with a file selected (`explorer /select,`) | Partly, and it is the classic breakage point [sourced: Opus "Application not found" thread] | No | No |
| This PC, Recycle Bin, Control Panel, libraries, phones | No | Yes (that is the downside of `Folder`) | Only the This PC window | No |
| Ctrl+E / "new window" from a folder window | Depends on which verb the caller uses (`explore` vs `open`) | Yes | No | No |
| Open/Save dialogs of other apps | Never | Never | Never | Never |

Two things worth internalising: **the taskbar and Start-menu Explorer icons are outside every mechanism
except IFEO/DLL patching**, and **"show in folder" is the action most likely to end up pointing at a stale
or missing exe** because it is the one third-party tools break when they forget to clean up (Total
Commander, OneCommander users, Directory Opus users all report it).

### 1.4 Do these survive Windows updates?

- `HKCU\Software\Classes` is user-profile data. Nothing in the sources shows a feature update resetting it
  [inferred]. No source found either way; treat as "survives in practice, unverified for 26H1".
- The **Win+E** half is the fragile part: Windows has changed how Win+E is dispatched, and Directory Opus
  users on 24H2 report it silently bypassing the CLSID key (June 2026), while Files' Win+E registration
  also broke for some users (#16111, #16251). [sourced]
- The **`Folder\shell` override** half is the part that breaks *other software*: it is the verb that
  "show in folder" and some installers call, and when it points at a missing exe the whole shell says
  "Application not found". [sourced]

---

## 2. What Defender and other AVs do about it (Q2)

### 2.1 ASR rules that could bite

From the ASR reference (all GUIDs quoted from it) [sourced: asr-rules-reference]:

| Rule | GUID | Relevance to us |
|---|---|---|
| Block executable files from running unless they meet a prevalence, age, or trusted list criterion | `01443614-cd74-433a-b99e-2ecdc07bfc25` | **The only rule that realistically hits us.** Needs cloud protection. A brand-new unsigned/signed-but-unprevalent `tsubame.exe` can be blocked on enterprise-managed machines. Nothing we do in the registry changes this; reputation does. |
| Use advanced protection against ransomware | `c1db55ab-c21a-4637-bb3f-a12568109d35` | Blocks files "that don't yet have a positive reputation", including signed ones. Same mitigation: prevalence. |
| Block untrusted and unsigned processes that run from USB | `b2b3f03d-6a65-4f7b-a9c7-1c7ef74a9ba4` | Only if the user runs the portable build from a USB stick / SD card. Worth a line in the portable README. |
| Block use of copied or impersonated system tools | `c0033c00-d16d-4114-a5a0-dc9b3a7d2ceb` | Heuristic; blocks executables that *look like* system tools running from unusual paths. Do not name anything in our install `explorer.exe`, `shell32.exe`, etc. |
| Block persistence through WMI event subscription | `e6db77e5-3df2-4cf1-b95a-636979351e5b` | Only if we used WMI for anything. We do not. |
| Block process creations originating from PSExec and WMI | `d1e49aac-8f56-4280-b9ba-993a6d77406c` | Only if the updater runs via WMI/scheduled task. Another reason the updater must be an in-process, user-initiated download. |
| Block all Office applications / Adobe / Office comms from creating child processes (`d4f940ab-…`, `7674ba52-…`, `26190899-…`) | — | Irrelevant, except: if a user opens our app *from* Outlook/Word, we are a child process of Office and these rules can block us. Known annoyance, not our bug. |
| Block rebooting machine in Safe Mode (`33ddedf1-…`) | — | Irrelevant. |

Notably **absent**: there is no ASR rule, in the current reference, that targets shell verb writes, class
registration, or IFEO. ASR is not the risk here. [sourced — the absence is in the reference table]

### 2.2 IFEO is actively detected — do not use it

- MITRE T1546.012, "Image File Execution Options Injection": tactics Persistence + Privilege Escalation,
  exploitation "cannot be easily mitigated with preventive controls", and Microsoft's own taxonomy lists
  "Unauthorized registry changes: modifications to the Windows Registry or system settings that impact the
  security posture of the device" under tampering software. [sourced: attack.mitre.org/…/T1546/012, criteria]
- Real Defender detection: `Behavior:Win32/ifeoDebugger.DB` — "A registry value is added under
  `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\.exe\Debugger` which
  triggers Defender behavior monitoring and removal." Reported April 2025 against PSAppDeployToolkit's
  `-BlockExecution`, on machines with Intune path exclusions applied; the toolkit maintainers deprecated
  the feature for 4.1.0 and removed it in 4.2.0 with the note "if endpoint security vendors are onto IFEO
  changes now, it's the nail in the coffin for this feature". [sourced: PSADT issue #1416]
- Elastic's prebuilt rule for the same technique carries explicit exceptions for ThinKiosk and PSADT —
  i.e. legitimate users of IFEO exist, and they are a maintained exception list, not a safe harbour.
  [sourced: elastic prebuilt rule page]

Consequence: redirecting `explorer.exe` via `IFEO\explorer.exe\Debugger` would (a) fire a known Defender
behavior detection on write, (b) get the key reverted and possibly the writing process quarantined,
(c) put us in the same bucket as SUNBURST and SDBbot. Reject.

### 2.3 The shell-verb key is also on the radar — but as a UAC bypass, not as a persistence trick

This is the most important and least pleasant finding:

- UACMe's author documents Defender behaviour signatures that fire on writes to
  `HKCU\Software\Classes\Folder\shell\open\command`: `Behavior:Win32/UACBypassExp.F!sdclt` (method 53),
  with the same family (`UACBypassExp.T`) covering `ms-settings` and `Launcher.SystemSettings`. He states
  the write event alone is inspected, and that the bypass mechanism is the CLSID-key *symbolic-link*
  variant of exactly this key. [sourced: swapcontext.blogspot.com, Oct 2020 — old but the only primary-ish
  account of the signature]
- Microsoft's own July 2026 threat report names the same key in the same role: ChocoShell "sdclt.exe folder
  hijack: Hijacks `HKCU\Software\Classes\Folder\shell\open\command` and launches the Windows Backup utility
  with the `/KickOffElev` flag", detected as "UAC bypass was detected / Possible Component Object Model
  (COM) hijacking". [sourced: Microsoft Security Blog, CaptiveCrunch, 2026-07-31]
- MITRE T1546.015 (COM hijacking) procedure examples include WarzoneRAT "setting the path to itself to the
  `HKCU\Software\Classes\Folder\shell\open\command` key with a `DelegateExecute` parameter". [sourced]

So the exact registry location we want is a documented attacker location, twice, by Microsoft. What keeps
Files/OneCommander/Directory Opus out of trouble is presumably context: the write comes from a user-launched
signed app, the command line points at a signed exe in a normal install path, and **no elevation-triggering
system binary is launched afterwards**. [inferred — no vendor documents this, and I found no report of
Defender flagging any of those four apps for it]

Practical consequences, in order of importance:

1. Prefer `Directory\shell` over `Folder\shell` (OneCommander's choice; avoids the exact UACMe path).
2. Write the key from the **non-elevated** app process during a user click. Never from an elevated helper,
   never from an installer, never in the same second as launching anything with `runas`.
3. Never set `DelegateExecute` on a verb we own. A plain `command` line is what legitimate file managers
   use; `DelegateExecute` + a CLSID of ours is the attack shape.
4. Keep the command line boring and absolute: `"C:\Program Files\Tsubame\tsubame-handler.exe" "%1"`.
   No `cmd /c`, no `powershell`, no `rundll32`, no `%TEMP%`.

### 2.4 Tamper Protection and Controlled Folder Access

- **Tamper Protection** protects Defender's *own* settings: real-time protection, behaviour monitoring, cloud
  protection, signature updates, automatic remediation, notifications, and the antivirus exclusion list;
  it also blocks "attempts to modify Microsoft Defender Antivirus settings through the registry". Its scope
  is Defender's configuration, **not** the shell class registry, so it cannot block our feature — and it
  also means we must never suggest "just exclude us", because on a tamper-protected machine the user
  cannot. [sourced: tamper-protection-overview]
- **Controlled Folder Access** only governs writes into protected folders (Documents, Desktop, Pictures …)
  by untrusted apps. Our feature writes to the registry, so CFA is not in the path; CFA would matter only
  for file *operations* inside protected folders (`IFileOperation` copy/delete), which is C3's problem, not
  P6's. [caveat: the CFA docs describe it that way, but I did not open the page in this session — §8 item
  40]
- **Smart App Control** is the one thing that can refuse to run us at all before any of this matters —
  see §3.4.

### 2.5 Real detection reports for "shell replacement" tools

The closest public evidence of AV reacting to shell modification comes from ExplorerPatcher, which injects
DLLs into Explorer and patches it — far more invasive than registry verbs, and therefore an upper bound on
what we should expect, not a prediction:

| Date | Detection | Source |
|---|---|---|
| Nov 2021 | `Trojan:Win32/Spursint.Q!cl` on `dxgi.dll` | EP issue 265, via explorerpatcher.dev timeline |
| Feb 2024 | `HackTool:Win64/Patcher` | EP 63.3 release notes |
| May 2024 | `HackTool:Win64/ExplorerPatcher!MTB` | EP issue #3228 (+ MS Q&A) |
| May 2024 | `Trojan:Win32/Vigorf.A` | EP issues #3225/#3229 |
| May 2026 | `Trojan:Script/Wacatac.h!ml` / `Wacatac.h!ml` on new releases | EP issues #4965, #5040 |
| Oct 2025 | "Defender no longer flags ExplorerPatcher" (they changed the code) | EP 69.4 notes |

I found **no** report of Defender, or any named AV, flagging Files, OneCommander, Directory Opus, Total
Commander or Q-Dir for writing shell verbs. That is the strongest evidence available that the registry
mechanism itself is not signatured, and it is evidence of absence. [inferred]

### 2.6 Per-user HKCU from a signed app vs HKLM from an installer

No Microsoft document says per-user writes are treated more leniently. What the documents and reports do
support:

- Defender's behaviour detections in this area are key- and process-shaped, not hive-shaped: PSADT's
  detection fired on an HKLM write performed by `powershell.exe`; the UACMe signature fires on a
  `HKCU` write. So "HKCU is safe" is not a rule. [sourced for both, inferred for the generalisation]
- Elevation is what makes a write *interesting*: an HKLM/HKCR write means the process is elevated, and the
  detections that combine "registry write" + "elevated system binary launched" all presuppose that.
  [inferred]
- A per-user write cannot break other accounts, cannot be reverted only by an admin, and cannot produce the
  "another user's folders are dead" support case. That is an argument from safety, not from detection.
- Microsoft's own criteria care about *consent and reversibility*, not about hive: "lack of choice" and
  "unauthorized registry changes" are what get software classified as unwanted or tampering. [sourced]

Conclusion: choose HKCU because it is honest and reversible and needs no admin, not because it is
invisible.

---

## 3. Code signing and reputation (Q3)

### 3.1 How SmartScreen decides

SmartScreen evaluates two signals on a downloaded file: **publisher reputation** (is it signed, by a
certificate with a history) and **file hash reputation** (has this exact file been downloaded a lot without
complaints). "Even when signed, a newly created binary could still show a SmartScreen warning until its
hash or publisher certificate accumulates sufficient evidence." Unsigned files must rebuild reputation from
zero on **every version**; signed files inherit certificate reputation across versions. There is **no
submission mechanism to buy reputation** for consumer endpoints; it builds organically, "several weeks and
hundreds of clean installs". [sourced: smartscreen-reputation]

| Certificate | First-download behaviour |
|---|---|
| Microsoft Store (MSIX) | No warning — Microsoft re-signs |
| OV or EV | Warning, but with the verified publisher name shown |
| Artifact Signing (ex-Trusted Signing) | Same as OV: "reputation builds over time; initial warnings expected" |
| Self-signed | Same as unsigned: strong block |
| None | "Windows protected your PC"; enterprise policy can remove the Run-anyway button entirely |

### 3.2 EV is dead for this purpose

"EV certificates previously bypassed SmartScreen entirely on first download … That behavior was removed in
2024. EV-signed files now go through the same reputation-building process as OV certificates." Paying the
EV premium "solely to avoid SmartScreen warnings is no longer justified". [sourced: code-signing-options,
smartscreen-reputation]

### 3.3 Microsoft Artifact Signing (formerly Trusted Signing), 2026 status

- GA since January 2026, rebranded from Trusted Signing to **Artifact Signing**; ~$9.99/month; no hardware
  token; integrates with GitHub Actions/Azure DevOps. [sourced: Tech Community GA post, code-signing-options]
- Public Trust eligibility (quickstart, as of Nov 2026): organisations in the US, Canada, EU, UK,
  Australia, New Zealand, Japan, South Korea, Singapore, Switzerland, Norway, Israel; **individual
  developers only in the US or Canada**, and the Azure billing account must have Account Type
  "Individual" with a legal name/address matching the government ID. [sourced: artifact-signing/quickstart]
- Identity validation takes 1–20 business days. Older "3 years of business history" guidance for
  organisations is no longer in the quickstart (a Nov 2026 MS Q&A answer states there are no minimum
  organisation-age restrictions); treat any age requirement as stale. [sourced, with the caveat that
  Microsoft's own pages have contradicted each other on this]
- **If the developer is outside the US/Canada as an individual, Artifact Signing public trust is not
  available** and the route is an OV certificate from a commercial CA ($150–300/yr, key on HSM/token since
  June 2023). [sourced: code-signing-options]

### 3.4 Smart App Control

"Smart App Control prevents users from running malicious applications by blocking untrusted **or unsigned**
applications … only allows processes to run if they're predicted to be safe"; it blocks "unknown script
files and macros from the web". Developer guidance is blunt: sign **all** binaries — exe, dll, temporary
installer files, scripts, **and uninstallers** — with a certificate from the Microsoft Trusted Root Program,
or sign with Artifact Signing. [sourced: smart-app-control pages]

On a Windows 11 machine where SAC is on, an unsigned Tsubame is blocked outright, and SAC **cannot be
turned back on** once off without reinstalling Windows [sourced: widely documented SAC behaviour — the
Learn page states SAC is off by default on new installs and only on for clean installs; the "can't re-enable"
detail is in the SAC FAQ, which I did not open this session]. Practical stance: SAC machines are a small
share, and the answer is "sign everything, including the updater payload", not "work around SAC".

### 3.5 Rust-specific false-positive patterns

The pattern is real and well documented in the Rust ecosystem, and it is **ML/heuristic, not signature**:

| Trigger | Evidence |
|---|---|
| `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true` — the exact release profile in `PLAN.md` §7 S0 | `Trojan:Script/Wacatac.B!ml` on a hello-world built with `rustc` (rust-lang/rust#88297, 2021); `Trojan:Win32/Wacatac.B!ml` / `Trojan:Script/Wacatac.H!ml` on release artifacts (foundry #6723; rtk #1633, where the maintainer's post-mortem lists exactly this profile as the cause) |
| Single-file statically linked binaries, no debug symbols | Same reports; the RTK post-mortem: these "produce binaries that look hardened to an ML classifier — the same profile malware authors use" |
| Binaries that spawn a second exe / intercept commands | RTK post-mortem names "spawning child processes, intercepting commands" as part of the match |
| Unsigned + fresh hash + downloaded | §3.1 — SmartScreen warning regardless of what the file does |
| Any packer (UPX etc.) | Not sourced in this session for Rust specifically, but every FP guide (NSIS wiki, Inno threads) says signing and *not* packing; UPX is the classic packer trigger [inferred/industry-standard] |

Patterns I could **not** source either way and must be treated as unknown:

- Direct `ntdll` usage (`NtQueryDirectoryFile`, as planned in `PLAN.md` P0). I found no report of a file
  manager being flagged for using native APIs. Direct syscalls / hook evasion are a different, flagged
  thing; calling documented-but-unofficial exports from `ntdll` is what Explorer itself does and what every
  fast enumerator does. [inferred: low risk, untested]
- `IFileOperation` / shell COM usage: no reports. Ships in every shell-integrated utility. [inferred: low]
- `wasmtime`-host exe (`tsubame-plughost.exe`): a small exe that loads a `.cwasm` from disk and runs it is
  *structurally* a loader. No evidence either way. [inferred: medium risk, and an argument for keeping the
  plug host a signed, version-pinned file inside the install dir, never a downloaded one]
- A tiny exe that spawns the main exe (our proposed handler shim): also structurally a loader. Mitigate by
  signing it, giving both files proper `VERSIONINFO` (CompanyName/ProductName/FileDescription), and having
  the shim spawn only one fixed, absolute path derived from its own module path.

Practical hygiene checklist (all cheap, all worth doing):

1. **Never UPX or otherwise pack.** Compression also makes Defender's on-access unpacking slower.
2. Keep `VERSIONINFO` (company, product, description, version, original filename) and the embedded manifest
   (`asInvoker`, `longPathAware`, PerMonitorV2) — `PLAN.md` S0b already plans the icon resource; add the
   rest to the same `build.rs`.
3. Sign the exe, the shim, the plug host, the installer **and the uninstaller** (§3.4).
4. Use one stable publisher identity for every release; changing certs resets publisher reputation.
5. Don't ship a self-extractor that writes an exe into `%TEMP%` and runs it — that is the highest-value
   single behaviour to avoid, and it also happens to be what the updater must not do (§4).
6. Submit a **release candidate**, not the shipped release, the first time you use a new installer
   technology or a new certificate (§3.6).
7. Put the SHA-256 of every artifact in the release notes so users (and analysts) can verify.

### 3.6 False-positive submission

- Portal: `https://www.microsoft.com/wdsi/filesubmission`, **sign in**, persona **Software Developer**
  (`?persona=SoftwareDeveloper`). No email submissions. Track at `wdsi/submissionhistory`. [sourced:
  submission-guide]
- Statuses are Submitted → In progress → Closed. Disputes go through the developer contact form attached to
  the result, not a resubmission. [sourced]
- **Microsoft publishes no turnaround commitment.** Files are scanned by automation immediately; analyst
  queue time is reported by users as days to ~a week, occasionally longer. Priority goes to prevalent files
  and SAID holders. Plan a submission *off* the release critical path. [sourced: submission-guide +
  anecdotal Q&A answers]
- Submit the artifact that was convicted — if the installer was flagged, submit the installer. State what
  the software does, that it is an opt-in, reversible shell-verb registration, and what installer/packer
  was used. [inferred from the portal fields + community guidance in the my-ssl write-up, which is
  secondary]
- Also expect third-party engines to disagree; a clean Defender verdict clears only Defender.

---

## 4. Installer and updater (brief is question 5) — summary, detail in `research-installer-updater.md`

- **MSIX is out** for the feature, for two independent reasons: (a) containerised registry — an MSIX app's
  `HKCU\Software\Classes` writes are copy-on-write into a per-package hive and "registry entries … are
  visible only inside its container", so the shell never sees them; file type associations and COM must be
  declared in the manifest instead [sourced: advancedinstaller MSIX registry chapter]; (b) both apps that
  tried the packaged route retreated — Files added a separate unpackaged launcher, OneCommander removed the
  feature from its Store build. [sourced]
- **Portable zip is the primary artifact** (no install, no registry, nothing to flag), **Inno Setup with
  `PrivilegesRequired=lowest` is the per-user installer**, with an optional
  `PrivilegesRequiredOverridesAllowed=dialog` machine-wide mode for people who want Program Files. Inno over
  NSIS because NSIS's own wiki documents "hundreds of false positives" caused by AVs signature-matching the
  NSIS *stub* itself. [sourced: NSIS_False_Positives, electron-builder #6347]
- Signing does not fix installer FPs: a **signed** (Azure Trusted Signing) Inno installer was flagged
  `Trojan:Win32/Wacatac.B!ml` in Feb 2025 and the author fixed it by *downgrading Inno Setup*. [sourced:
  JUCE forum thread] So: pin the installer-compiler version, test each release on the VM, and keep the zip
  as the fallback channel.
- **Updater rules** (design, [inferred] except where noted): HTTPS only; GitHub Releases API as the only
  source; download to `%LOCALAPPDATA%\Tsubame\updates\<version>\`, verify SHA-256 against a manifest *and*
  an Ed25519/minisign signature with a public key compiled into the exe **[design choice — a hash list
  downloaded from the same server proves nothing]**; then verify the Authenticode signature and publisher
  of the new exe **[sourced rationale: unsigned files cannot inherit reputation, and a modified signed file
  breaks its signature — smartscreen-reputation]**; stage by renaming the running exe to `*.old` and the
  new one into place (Windows allows renaming a running image, not overwriting it); restart by launching
  the new exe from its final path, never from `%TEMP%`; delete `*.old` on next start; no `powershell`,
  `cmd /c`, `reg.exe`, `msiexec /q` one-liners; no scheduled task, no service, **no persistence of any kind
  other than the registry keys the user explicitly enabled**.

---

## 5. Recommendation for "replace default file explorer" (question 6)

### 5.1 Mechanisms, in implementation order

**Tier 0 — ship first, no risk, no admin, no registration**
- `Directory\shell\Tsubame` = "Open in Tsubame", `Directory\Background\shell\Tsubame` (empty space),
  `Drive\shell\Tsubame`. Opt-in checkbox, off by default. Adds a context-menu entry; does not change any
  default. This alone satisfies most users ("I can right-click a folder and open it in Tsubame") and gives
  us the whole registry code path — write, verify, remove — in a place where a bug cannot lock anyone out.

**Tier 1 — the default verb (the actual feature)**
```
HKCU\Software\Classes\Directory\shell
    (Default) = "Tsubame"                  ; previous value saved in Directory\shell\Tsubame\PreviousDefault
HKCU\Software\Classes\Directory\shell\Tsubame
    (Default) = "Open in Tsubame"
    Icon      = "<install>\tsubame.exe,0"
HKCU\Software\Classes\Directory\shell\Tsubame\command
    (Default) = "\"<install>\tsubame-handler.exe\" \"%1\""
```
No `DelegateExecute`, no `DropTarget`, no `CLSID` key, nothing under `Folder`. Covers double-click, the Run
dialog, `start <path>` and browser "open folder". Does not touch drives, virtual folders, dialogs, the
taskbar or the Start-menu icon.

**Tier 2 — Win+E, separate checkbox, opt-in**
```
HKCU\Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}\shell\opennewwindow\command
    (Default)         = "\"<install>\tsubame-handler.exe\" --this-pc"
    DelegateExecute   = ""     ; explicit empty string neutralises the COM handler
```
Ship it labelled "experimental on Windows 11 24H2 and later" because it is: Directory Opus users report it
silently failing there, and Files' version of it broke for some users. Undo = delete only that CLSID key,
and only if its command still points at us.

**Tier 3 — `Folder\shell` override, only if Tier 1 proves insufficient on the VM**
If we ever do it: same shape as Tier 1 but under `Folder`, with the same save/restore of the previous
default, and with the UAC-bypass analysis from §2.3 written into the code comment. Do not do this in the
first release.

**Never, under any circumstances**: `IFEO\explorer.exe\Debugger`; `HKLM`/`HKCR` writes; any
`CLSID\{...}\InprocServer32`/`LocalServer32` of ours; DLL injection into `explorer.exe`; replacing or
renaming `explorer.exe`; `ShellEx` handlers; `UserChoice` keys; `DelegateExecute` pointing at our own CLSID.

### 5.2 The handler shim, and failing safe

`tsubame-handler.exe` — one small Rust binary, no window, signed, same `VERSIONINFO` as the app:

1. Parse argv: a folder path, or `--this-pc`.
2. Sanitise: reject empty, reject anything that is not an existing directory or one of the two
   known-good `::{CLSID}` names we accept for This PC. Anything else → hand straight to `explorer.exe`, so
   we never become the thing that turns an odd shell namespace into "Application not found".
3. Hand off: if an instance is running, send the path (named pipe or `WM_COPYDATA` to a message-only
   window) and exit; else `CreateProcess("<install>\tsubame.exe", "--open <path>")`, wait for input idle
   with a timeout (~2 s), and exit.
4. If the process failed to start, exited non-zero within the timeout, or the main exe is missing:
   **remove our own registration** (the exact inverse of §5.4, from the same code), log to
   `%LOCALAPPDATA%\Tsubame\handler.log`, and `ShellExecute("explorer.exe", "\"%1\"")`.

That last step is the whole safety story: the registration is designed so that the *failure mode of the
registration itself* is "Explorer opens, and the feature switches itself off". A user who drags the install
folder to the Recycle Bin gets one Explorer window and a clean registry, not a dead shell. It also means a
Defender quarantine of `tsubame.exe` cannot lock anyone out — the shim survives (different file), notices,
falls back, and deregisters.

Residual risk that cannot be designed away: **if the shim itself is deleted while it is the registered
handler**, folders error out. Mitigations: (a) install into a directory users do not casually edit
(`%LOCALAPPDATA%\Programs\Tsubame` per-user or `%ProgramFiles%\Tsubame` machine-wide); (b) the uninstaller
restores Explorer *before* deleting any file, and verifies by re-reading the keys; (c) the app re-checks and
repairs a stale path on every start; (d) a `tsubame --restore-explorer` switch, a `restore-explorer.reg`
next to the exe, and the same file on the download page; (e) the README's recovery line:
`Win+R` → `explorer.exe`. Test case T9 below covers it.

### 5.3 What the UI must say

One screen, three independent checkboxes, all off by default, plus a "Restore Windows Explorer" button that
is always visible (not hidden behind "advanced"):

- Title: **"Use Tsubame instead of File Explorer for folders"**
- Body (exact intent, not exact words): "Windows has no supported setting for this. Tsubame makes a
  per-user registry change that makes folders open in Tsubame. It only affects your Windows account, needs
  no administrator rights, and the button below undoes it."
- Under the "Open folders in Tsubame" checkbox: "Double-clicking a folder, the Run dialog and 'open folder'
  in other apps will open Tsubame. Drives, This PC, the Recycle Bin, Control Panel, the taskbar Explorer
  icon and any app's Open/Save dialog keep opening File Explorer — Windows does not let us replace those."
- Under "Win+E": "Experimental on Windows 11 24H2 and later. If Windows ignores it, use the other options."
- Under "Add Open in Tsubame to the folder context menu": no warning needed.
- A live status line: "Current default folder handler: Tsubame | File Explorer | Another app (…)" with the
  actual registry value, and a "Refresh" — so we never claim to be the default when we are not.
- On quit/uninstall, nothing about this screen changes by itself, except: if the registry no longer points
  at us, the checkboxes show unchecked and a note says another app changed it. **We never re-assert.**

### 5.4 One-click reversal, uninstall, and a deleted app

The undo routine is the mirror image, and it must be idempotent and total:

1. If `Directory\shell\Tsubame\PreviousDefault` exists → restore it to `Directory\shell\(Default)`
   (preserving its type); else delete `Directory\shell\(Default)`.
2. Delete `Directory\shell\Tsubame` (the whole subtree).
3. Delete `Directory\shell` only if it has no remaining subkeys and no default value; same for
   `Directory\Background\shell`, `Drive\shell`.
4. Delete the `{52205fd8-…}` CLSID subtree **only if** its `opennewwindow\command\(Default)` still contains
   our shim path. If another app has taken it over, leave it and report that.
5. `SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST|SHCNF_FLUSH, null, null)` so Explorer refreshes.
6. Re-read everything and only then show "Restored".

Uninstall path: run this **first**, before touching files; if it fails, keep the files and tell the user
which key is stuck (never delete the exe that the registry still points at). The Inno uninstaller deletes
files only after the restore step returns success. Also ship the standalone `restore-explorer.reg` for the
"app already gone" case, and reference it from the download page and the README.

### 5.5 Test matrix for a clean Windows 11 VM, Defender on (do not run — for the P6 task)

Build the VM fresh per group so state does not leak.

**A. Baseline / no regression**
1. Fresh install, all three options off: double-click, Win+E, Run `C:\`, browser "open folder", "show in
   folder", Open/Save dialog all behave exactly like stock. `HKCU\Software\Classes` has no `Tsubame` keys.
2. Portable zip, run from a folder, all options off: same, and nothing is written outside
   `%LOCALAPPDATA%\Tsubame` (verify with a registry diff of HKCU before/after).
3. Uninstall with options off: no leftover keys, no leftover files, no `Run` key, no scheduled task.

**B. Each option alone**
4. Tier 1 on → double-click folder, Run dialog, `start C:\x` from a terminal, Chrome "open folder" → all
   open Tsubame at that path; "show in folder" → Explorer (documented behaviour, not a bug); taskbar icon →
   Explorer.
5. Tier 2 on (+ Tier 1 off) → Win+E opens Tsubame at This PC; note whether it works on 24H2/25H2 and record
   the exact build. Repeat after a Windows Update.
6. Tier 0 on → context menu entry appears on folders, folder background, drives; nothing changed as default.
7. Tier 1 on, then start Tsubame twice from two folders quickly → second call goes to the existing instance
   (no duplicate window, no lost path).

**C. Reversal**
8. Tier 1 on → "Restore Windows Explorer" → registry byte-identical to the pre-change export; folders open
   in Explorer immediately (no sign-out).
9. Tier 1 on with a *pre-existing* third-party default (e.g. set `Directory\shell` default by hand to a
   dummy verb first) → turning ours on then off restores the dummy verb, not "nothing".
10. Tier 1 + Tier 2 on, then uninstall with the app running → uninstaller asks to close, restores
    Explorer, then deletes files; verify by re-reading both keys and by opening a folder.

**D. Hostile / failure cases**
11. Tier 1 on, then **kill and delete the whole install folder** → open a folder: one Explorer window opens,
    the `Directory\shell` keys are gone afterwards, `handler.log` explains it. (This is the T9 case that
    matters most.)
12. Tier 1 on, then rename `tsubame.exe` → same as 11 (shim falls back and deregisters).
13. Tier 1 on, then make `tsubame.exe` hang at startup → after the shim timeout, Explorer opens the folder
    and the registration is removed.
14. Tier 1 on, then set `Directory\shell\(Default)` to something else by hand → Tsubame's Settings shows
    "another app", does not fight it, and its checkbox unticks on refresh.
15. Tier 1 on, then a second file manager (Files or OneCommander) takes over → both apps still work,
    Explorer still works, and uninstalling either does not break folders.

**E. Defender**
16. Windows 11 24H2/25H2, Defender fully updated, real-time + cloud + PUA + tamper protection on, SAC **on**
    in a separate VM: install unsigned RC → record SAC block; then signed RC → install and toggle Tier 1 on;
    check **Protection history** immediately after each write for any behaviour detection (look for
    `Behavior:Win32/…`). Repeat with an ASR policy applied via PowerShell (`Set-MpPreference`) with
    `01443614-…` and `c1db55ab-…` in block mode, to see the worst enterprise case.
17. Same VM: run the updater end to end; confirm the new exe is not written to `%TEMP%`, that the old exe is
    renamed not overwritten, and that Protection history stays clean.
18. Submit the RC (installer + exe) to `wdsi/filesubmission` as Software Developer before public release;
    record the verdict and turnaround. Re-submit after switching installer version or certificate.
19. Copy the portable build to a USB stick and run it from there (ASR `b2b3f03d-…` scenario) and note the
    result in the README.

**F. Regression**
20. After each Windows cumulative update: repeat 4, 5, 9 and 11 (the four cases Windows can break).

---

## 6. Risk table

Risk is *detection or user-harm risk*, given an opt-in, per-user, signed, reversible implementation.

| Mechanism | Detection risk | Why | Reversibility | Admin |
|---|---|---|---|---|
| `Directory\shell\Tsubame` + default verb (Tier 0/1) | **Low** | OneCommander uses this exact path; Files/Do­pus/Total Commander use the equivalent verb one class up. No ASR rule applies; it is not the documented UAC-bypass key. No published detection found. | Full; one registry delete, no reboot | No |
| `Directory\Background` / `Drive` context entries (Tier 0) | **Very low** | Additive only, no default changed | Full | No |
| `CLSID\{52205fd8-…}\shell\opennewwindow` (Win+E, Tier 2) | **Low–medium** | Writes into a `CLSID` subtree, which is the shape COM-hijack detections look at; does not create `InprocServer32`. Functionally unreliable on 24H2+. | Full | No |
| `Folder\shell\open\command` override (Files-style, Tier 3) | **Medium** | Exact key of `Behavior:Win32/UACBypassExp.F!sdclt` (UACMe 53) and of ChocoShell's sdclt bypass in Microsoft's 2026 report; also the key that breaks "show in folder" if it goes stale | Full, but touches the generic class everyone shares | No |
| Machine-wide `HKCR`/`HKLM` variant from an installer | **Medium–high** | Requires elevation; the elevated-write + shell-key shape is what the UAC-bypass detections are built on; affects all users; Total Commander's leftovers show how bad a stale one is | Full, but only by an admin | **Yes** |
| `IFEO\explorer.exe\Debugger` | **Very high** | `Behavior:Win32/ifeoDebugger.DB` reverts the key and can quarantine the writer; T1546.012; PSADT removed the feature | Possible but may be reverted by Defender first | Yes |
| `CLSID\{…}\InprocServer32` of ours / shell extension DLL | **Very high** | Canonical T1546.015; ExplorerPatcher's DLL work earned `HackTool:Win64/ExplorerPatcher!MTB` | Poor | No (HKCU) / Yes |
| Patching / injecting / replacing explorer.exe | **Very high** | `Trojan:Win32/Spursint.Q!cl`, `HackTool:Win64/Patcher`, `Trojan:Win32/Vigorf.A`, `Wacatac.h!ml` all reported against ExplorerPatcher | Poor, and can leave a machine with no shell | Yes |
| MSIX package "owning" folder associations | n/a | Cannot work: containerised registry; Microsoft Store policy has no concept of a default file manager | — | No |
| `UserChoice` file-extension hijack | n/a / **high** | Not applicable to folders, and programmatic default-setting for extensions was deprecated in Windows 8: the supported path is only to launch the Settings UI. Trying anyway is the classic "hijacker"/PUA pattern. | — | — |

---

## 7. What I could not verify, and where the sources are thin

- **No ASR or Defender detection explicitly targeting `Directory\shell`/`Folder\shell` writes by legitimate
  apps.** The nearest primary evidence is the UACMe write-up (2020, one author, describes signatures whose
  current names may differ) and Microsoft's 2026 report that names the key as an attack technique. The
  claim "our variant is not detected" rests on the absence of reports for four shipping apps plus the
  contextual argument in §2.3. **This is the main thing to test on the VM (test 16).**
- **Explorer++'s implementation**: not found; excluded from all claims.
- **XYplorer's** exact keys: one user report only.
- **Whether Directory Opus writes HKLM or HKCU** for `Folder\shell`: the forum evidence shows the merged
  HKCR view; the manual only says "requires changes to the Windows registry".
- **Windows update survival** of `HKCU\Software\Classes` in general: no source found either way; the Win+E
  half is documented as fragile.
- **Artifact Signing minimum business age**: Microsoft's own pages conflict (quickstart vs older Q&A). The
  current quickstart has no age requirement; do not plan on that staying true.
- **Smart App Control's exact verdict rules** are not published; "unsigned unless positively reputable is
  blocked" is the documented summary, and the granularity (per-file hash vs per-publisher) is not.
- **SmartScreen evaluation of a process launched by our own updater** (as opposed to by the shell) is not
  documented anywhere I found. Design as if it applies: assume the download is MOTW-marked and will be
  checked when launched. [inferred]
- Everything about `tsubame-handler.exe`'s spawn cost is unmeasured. It is one extra short-lived process on
  the "open a folder" path; measure it against the 100 ms first-paint budget in `PLAN.md` §2 before
  committing to the shim, and if it costs more than ~15 ms consider registering `tsubame.exe --open %1`
  directly and accepting the weaker fallback.

---

## 8. Sources

1. ASR rules reference (GUIDs, per-rule detail) — https://learn.microsoft.com/en-us/defender-endpoint/attack-surface-reduction-rules-reference
2. Microsoft Defender SmartScreen overview — https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/
3. Code signing options for Windows app developers (OV/EV/Artifact Signing, EV no longer bypasses SmartScreen) — https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options
4. SmartScreen reputation for Windows app developers — https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
5. Smart App Control — https://learn.microsoft.com/en-us/windows/security/application-security/application-control/smart-app-control/
6. Artifact Signing overview — https://learn.microsoft.com/en-us/azure/artifact-signing/overview
7. Artifact Signing quickstart (eligibility, individual developers US/CA) — https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart
8. Trusted Signing is now Artifact Signing, GA (Jan 2026) — https://techcommunity.microsoft.com/blog/microsoft-security-blog/simplifying-code-signing-for-windows-apps-artifact-signing-ga/4482789
9. Default Programs (Win32): API deprecated in Win8, "user driven", UI is the supported path — https://learn.microsoft.com/en-us/windows/win32/shell/default-programs
10. Tamper protection overview — https://learn.microsoft.com/en-us/defender-endpoint/prevent-changes-to-security-settings-with-tamper-protection
11. Submit files for analysis (persona, statuses, no turnaround commitment) — https://learn.microsoft.com/en-us/unified-secops/submission-guide
12. How Microsoft identifies malware and potentially unwanted applications (criteria: lack of choice/control, tampering, unauthorized registry changes) — https://learn.microsoft.com/en-us/unified-secops/criteria
13. MITRE ATT&CK T1546.012 Image File Execution Options Injection — https://attack.mitre.org/techniques/T1546/012/
14. MITRE ATT&CK T1546.001 Change Default File Association (incl. DET0061 covering HKCU and HKCR) — https://attack.mitre.org/techniques/T1546/001/
15. MITRE ATT&CK T1546.015 COM Hijacking (WarzoneRAT: `HKCU\Software\Classes\Folder\shell\open\command` + `DelegateExecute`) — https://attack.mitre.org/techniques/T1546/015/
16. PSAppDeployToolkit #1416 — `Behavior:Win32/ifeoDebugger.DB`, feature deprecated (2025) — https://github.com/PSAppDeployToolkit/PSAppDeployToolkit/issues/1416
17. Elastic prebuilt rule: Image File Execution Options Injection (ThinKiosk / PSADT exceptions) — https://www.elastic.co/docs/reference/security/prebuilt-rules/rules/windows/persistence_evasion_registry_ifeo_injection
18. UACMe 3.5, Windows Defender and the ways of mitigation (WD behaviour signatures on `HKCU\...\Folder\shell\open\command`) — https://swapcontext.blogspot.com/2020/10/uacme-35-wd-and-ways-of-mitigation.html
19. Microsoft Security Blog, CaptiveCrunch / Midnight Blizzard (2026-07-31): ChocoShell sdclt folder hijack on `HKCU\Software\Classes\Folder\shell\open\command` — https://www.microsoft.com/en-us/security/blog/2026/07/31/captivecrunch-midnight-blizzard-targets-travelers-worldwide-for-malware-delivery-and-credential-theft/
20. Files #16111 — the exact keys Files writes, and the Win+E failure — https://github.com/files-community/Files/issues/16111
21. Files #7626 — "if you uninstalled Files before turning off the option…" recovery script — https://github.com/files-community/Files/issues/7626
22. Files docs: Replacing File Explorer with Files (unsupported) — https://github.com/files-community/files-community.github.io/blob/main/docs/articles/replace-file-explorer.md
23. OneCommander — Advanced settings ("Register as a default file manager": experimental) — https://onecommander.com/help2/Advanced.html
24. OneCommander — Uninstalling (must uncheck first) — https://onecommander.com/help2/Uninstalling.html
25. OneCommander support thread — the two keys OC sets, and why `Directory` not `Folder` — https://groups.google.com/g/onecommander/c/dOTHKRiYXVg
26. OneCommander support thread — `RestoreExplorerAsTheDefaultFileManager.reg`, conflicts with other managers — https://groups.google.com/g/onecommander/c/cbd4WyJ9h8c
27. OneCommander author: feature removed from the Store/MSIX build — https://www.reddit.com/r/OneCommander/comments/1hcdk4k/how_to_make_it_default_File_manager_in_windows/
28. Directory Opus manual: Explorer Replacement — https://docs.dopus.com/doku.php?id=basic_concepts%3Aexplorer_replacement
29. Directory Opus manual: Explorer Replacement preferences (incl. Win+E on a separate page) — https://docs.dopus.com/doku.php?id=preferences%3Apreferences_categories%3Alaunching_opus%3Aexplorer_replacement
30. Directory Opus forum (June 2026): Win+E ignored on Win11 IoT LTSC 24H2, `HKCR\Folder\shell` = `openindopus` — https://resource.dopus.com/t/win-e-shortcut-opens-windows-11-file-explorer-instead-of-directory-opus-13/59625
31. Directory Opus forum: "show in folder" / "Application not found", and the Total Commander keys that caused it — https://resource.dopus.com/t/chrome-show-in-folder-and-desktop-folders-show-application-not-found/40607
32. Superuser: replacing Explorer, the `{52205fd8-…}` `opennewwindow` + `DelegateExecute=""` recipe (Q-Dir) — https://superuser.com/questions/241442/how-would-i-replace-default-windows-explorer-with-an-alternative-one-say-q-dir
33. KDE forum: "Setting Dolphin as the default file manager on Windows 11" — the same recipe for Dolphin, with
    the revert script (URL as indexed by search; the thread also exists under `forum.kde.org` viewtopic
    `f=59&t=173363`) — https://forum.kde.org/viewtopic.php?f=59&t=173363
34. How-To Geek: changing the default file explorer (File Pilot has no option; HKCR recipe; OC author's correction to HKCU in the comments) — https://www.howtogeek.com/how-and-why-to-change-the-default-file-explorer-in-windows/
35. strontic: CLSID `{52205fd8-5dfb-447d-801a-d0b52f2e83e1}` = This PC / shell folder, `OpenNewWindow` verb — https://strontic.github.io/xcyclopedia/library/clsid_52205fd8-5dfb-447d-801a-d0b52f2e83e1.html
36. NSIS wiki: NSIS False Positives ("hundreds of false positives", stub signatures, "sign your installer… reports it helps") — https://nsis.sourceforge.io/NSIS_False_Positives
37. electron-builder #6347 — NSIS installer flagged by Defender — https://github.com/electron-userland/electron-builder/issues/6347
38. JUCE forum: a **signed** (Artifact Signing) Inno installer flagged `Trojan:Win32/Wacatac.B!ml`, fixed by downgrading Inno — https://forum.juce.com/t/proactive-measures-to-prevent-inno-setup-installer-from-getting-flagged-by-windows-defender/65217
39. innosetup group: 6.2.1 installers flagged, 6.2.0 not; "submit to the vendors" — https://groups.google.com/g/innosetup/c/lvsb2vWhklk
40. Controlled folder access docs (cited, not opened this session) — https://learn.microsoft.com/en-us/defender-endpoint/controlled-folders
41. rust-lang/rust #88297 — `Trojan:Script/Wacatac.B!ml` on a hello-world rustc build — https://github.com/rust-lang/rust/issues/88297
42. foundry #6723 — `Trojan:Script/Wacatac.B!ml` on release binaries — https://github.com/foundry-rs/foundry/issues/6723
43. rtk #1633 — `Trojan:Script/Wacatac.H!ml`, maintainer post-mortem naming LTO/`codegen-units=1`/`panic=abort`/`strip` — https://github.com/rtk-ai/rtk/issues/1633
44. ExplorerPatcher #3228 — `HackTool:Win64/ExplorerPatcher!MTB` — https://github.com/valinet/ExplorerPatcher/issues/3228
45. ExplorerPatcher #4965 — `Wacatac.h!ml` on a 2026 release — https://github.com/valinet/ExplorerPatcher/issues/4965
46. ExplorerPatcher detection timeline (secondary, cites the issues) — https://explorerpatcher.dev/antivirus/
47. MSIX registry containerisation (Advanced Installer / Tim Mangan chapter): "registry entries of an MSIX-packaged app are visible only inside its container", FTA/COM must be in the manifest — https://www.advancedinstaller.com/application-packaging-training/msix-packaging/ebook/registry.html
48. MSIX package signing overview (Artifact Signing recommended for production MSIX) — https://github.com/microsoftdocs/msix-docs/blob/main/msix-src/package/signing-package-overview.md
