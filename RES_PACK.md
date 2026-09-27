# OfficialV2 resource packs

`res_pack` replaces `fontpack` and `iconpack`; the old type names are no longer accepted.

## Package artifact & layout

A resource pack is a standard `.zip` file artifact declared in `manifest.downloads`, e.g. `dark.zip`.
The provider downloads this `.zip` file through the normal download manager into local cache like any other resource file.

Inside the ZIP archive:
- Contains a `mappings.tsv` and all theme resource files.
- Either packaged directly at root (`mappings.tsv`, `icons/...`, etc.), where `themeId` is matched from `themes/<themeId>/` target paths in `mappings.tsv` or the archive file stem;
- Or all files reside under a top-level directory `<themeId>/` (`<themeId>/mappings.tsv`, `<themeId>/icons/...`, etc.).
- `themeId` is 1–12 lowercase ASCII letters, digits, `_` or `-` (e.g. `dark`).
- Unsafe paths, symlinks, missing `mappings.tsv`, more than 4096 files or more than 64 MiB total unpacked data are rejected.

## Installation via Interconnect

- Every resource-pack manifest gets an `OfficialV2` resource dependency on `ng.lst.conora` in `ext.bundledResources.required`, preserving other dependencies.
- The installation queue checks the target device. If missing, it downloads and installs the compatible Conora quick app as a prerequisite within that queue task; failure prevents the pack transfer.
- During installation, the `.zip` file is unpacked in memory by corelib, `ng.lst.conora` is launched on the wearable, and the extracted files are sent individually over interconnect according to the draft protocol. Resource packs never use the normal Mass file-install route. Currently supported on native Xiaomi devices.
- Protocol v1 uses H/T/P/F/A/C/E, at most 18,000 characters per complete message and a window of at most four chunks. Payload chunks default to 12,000 bytes, reduced for a smaller negotiated text limit. ACKs mean receiver-side writes, not Bluetooth/platform send completion.
- Each control request waits for the matching acknowledgement. Chunks have independent acknowledgement deadlines; only unconfirmed chunks are retried. Each request/chunk has at most four attempts, eight seconds per acknowledgement wait.
- A retry of the **same installation task** uses `resume`, the exact source fingerprint and the original chunk size, and honors `receivedRanges`. Starting a new task uses `replace`.
- A package is successful only after each file's C acknowledgement and the final `T finish` ready response.
- `mappings.tsv` is sent as an ordinary file (ordered first at `fileIndex: 0`), never interpreted or rewritten. Activation remains a Manager operation. `active-theme` is an error: switch to another theme/default on-device before replacing the active package.

## Base91 interoperability

The variant is Joachim Henke's standard basE91. Each chunk is encoded independently with this 91-character alphabet:

```text
ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&()*+,./:;<=>?@[]^_`{|}~"
```

Test vectors: empty bytes encode to an empty string; UTF-8 `Hello World!` encodes to `>OwJh>Io0Tv!8PE`.
