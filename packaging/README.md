# Packaging

Feather is a single binary. Everything here registers that binary with the
operating system — it does not install a runtime, a service, or a directory of
shared libraries, because there are none.

| Platform | Script | What it does |
|---|---|---|
| Linux | `linux/install.sh` | Copies the binary to `~/.local/bin`, writes a `.desktop` entry with the MIME types we can actually open, refreshes the desktop database. |
| Windows | `windows/install.ps1` | Writes `HKCU` registry entries: a ProgID, an `Applications` entry, `Capabilities` for Settings > Default apps, and `OpenWithProgids` for each extension. No admin rights, no installer. |
| macOS | `macos/README.md` | Not automated yet. Explains the `.app` bundle, the `Info.plist` document types, and why notarisation is the real blocker. |

## One rule all three follow

**Register as a candidate; never seize the default.** Every script stops short of
making Feather the default handler. The user chooses that, in their own settings,
where they can also undo it. §7 of the plan argues this at length — an
application that grabs file associations on install is the reason people
distrust installers.

## Uninstalling

Every script is reversible:

```sh
packaging/linux/install.sh --uninstall
```

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1 -Uninstall
```

## CI

`packaging/linux/install.sh` runs in CI against a temporary prefix on every push:
it installs, asserts the `.desktop` entry exists, parses, advertises the formats
the router supports, and points `Exec` at the binary — then uninstalls and
asserts the entry is gone. A packaging script that only works on the machine it
was written on is not a packaging script.
