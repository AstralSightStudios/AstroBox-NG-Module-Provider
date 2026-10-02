# OfficialV2 resource packs

`res_pack` replaces `fontpack` and `iconpack`; the old type names are no longer accepted.

Authoritative format contract: `Canopus-Module-Resource-Hook/docs/interconnect_proto.md` ("CRPack v1").
Reference authoring implementation: `leset0ng/Conora-editor` (`crates/corona-core/src/crpack.rs`).

## Package artifact

A resource pack is a ZIP container declared in `manifest.downloads`, conventionally `dark.crpack`.
The extension is only a hint and is never used for recognition: the sender must confirm the file is
a readable ZIP whose root holds a size-bounded `corona.json` carrying the exact format marker.
The provider downloads the artifact through the normal download manager into local cache like any
other resource file; it never inspects or rewrites the container.

Inside the archive:

```text
dark.crpack  (ZIP)
├── corona.json               # metadata + mapping rules; also transferred to the device
├── app/launcher/*.bin
└── icons/confirm.bin
```

- Exactly one root `corona.json`, stored as an ordinary file, never as a directory entry.
- Resource files live directly under relative paths at the archive root. There is no wrapper
  directory, and the ZIP must not be nested one level deeper.
- `mappings.tsv` is **forbidden** inside the archive. It is a *derived* file: after receiving a
  pack the device Manager generates `themes/<themeId>/mappings.tsv` from `corona.json`, and the
  home-screen reload generates the active `internal://files/mappings.tsv` from the saved pack order
  and the "system style" divider. Two mapping sources in one package are rejected by design.
- Rejected: encrypted entries, symlinks and other special files, duplicate paths, absolute paths,
  backslashes, empty segments, `.` / `..`, out-of-bounds paths, an oversized manifest, or content
  exceeding the unpacked budget. Unsafe entries are never "repaired" by path normalization. Only
  ZIP Store/Deflate is supported, CRCs are verified, and the byte budget is enforced against the
  actually decompressed size rather than the size declared in the ZIP directory.

## `corona.json`

UTF-8 JSON without a BOM, at most 64 KiB, root object. `format`, `formatVersion`, `themeId`, `name`
and `mappings` are required; `version`, `author`, `description`, `targets` and `quickappIcons` are
optional and are display-only metadata that never affect resumption or device compatibility.

| Field | Constraint |
|---|---|
| `format` | exactly `"canopus-resource-pack"` |
| `formatVersion` | integer `1` |
| `themeId` | 1–12 chars, lowercase ASCII letters, digits, `_` or `-` (e.g. `dark`); also used verbatim as the device theme directory name and as the protocol `themeId` |
| `name` | non-empty, ≤128 UTF-8 bytes, no control characters |
| `mappings` | array of `{source, destination}`; may be empty |
| `version` | ≤64 UTF-8 bytes |
| `author` | ≤128 UTF-8 bytes |
| `description` | ≤1024 UTF-8 bytes; control characters other than TAB/CR/LF rejected |
| `targets` | ≤16 strings, each ≤128 UTF-8 bytes; author-declared only, not a compatibility claim |
| `quickappIcons` | array of `{package, destination}` |

Mapping rules:

- `source` is either an absolute firmware resource path or a `@quickapp-icon/<package>` semantic key,
  each under 255 UTF-8 bytes. Ordinary directory and file rules must agree on the trailing slash.
- `destination` is a safe relative path inside the archive that corresponds to an actual packaged
  file. QuickApp destinations must be real lowercase `.bin` files, never directories or PNGs.
- Mapping fields reject ASCII control bytes (including NUL, TAB, CR, LF) and DEL.
- `mappings` and `quickappIcons` combined are capped at 256 rules, serialized in order.
- `package` is an opaque exact string: no trimming, no ASCII or dotted-grammar requirement, no path
  interpretation. A trailing `/` is part of the name and never forms a directory rule. The
  `@quickapp-icon/` prefix plus the raw package must stay within 255 UTF-8 bytes, and bytes 0–31
  and 127 are forbidden. Duplicate package keys are rejected.
- The Manager serializes each rule as `source<TAB><quickapp-file-area relative destination><LF>`
  and requires the generated `mappings.tsv` to stay within 32 KiB. The mapped device path
  `themes/<themeId>/<destination>` must be shorter than 256 UTF-8 bytes.

## Limits

| Budget | Value |
|---|---|
| Total uncompressed content, including `corona.json` | 64 MiB |
| `corona.json` | 64 KiB |
| Archive / theme file count | no container limit; a single Interconnect transfer is capped at 65,536 files by the 4-digit hexadecimal file index, and each file must fit the negotiated chunk size and the local 2,048-chunk cap |
| `mappings` + `quickappIcons` | 256 rules |
| Generated `mappings.tsv` | 32 KiB |
| Mapped device path | <256 UTF-8 bytes |
| `themeId` | 1–12 characters |

The container imposes no file-count cap; the 128-file Manager limit from the first generation has
been removed on the receiver, and the sender no longer enforces it either. The only remaining
caps are the Interconnect file-index encoding (65,536 files per transfer, counted in hexadecimal
with four digits, `corona.json` included) and the ZIP central-directory sanity bound in `core`
(`MAX_ARCHIVE_ENTRIES`).

## Installation via Interconnect

- Every resource-pack manifest gets an `OfficialV2` resource dependency on `ng.lst.corona` in
  `ext.bundledResources.required`, preserving other dependencies. The identifier is the Canopus
  module id from `Canopus.toml` (`[module] id`); the runtime/receipt identifier is `corona`. It has
  historically been mistyped as `ng.lst.conora`, which resolves to nothing.
- The installation queue checks the target device. If missing, it downloads and installs the
  compatible corona Manager quick app as a prerequisite within that queue task; failure prevents
  the pack transfer.
- During installation corelib unpacks the container in memory, launches `ng.lst.corona` on the
  wearable, and sends `corona.json` plus every resource file individually over interconnect
  according to the draft protocol. Resource packs never use the normal Mass file-install route.
  Currently supported on native Xiaomi devices only.
- `corona.json` is transferred verbatim as file index `0` and saved unmodified into the theme
  directory; the transport layer never interprets or rewrites it, never renames or reorders
  resource paths. The receiver validates that `corona.json` agrees with the transferred `themeId`
  before registering the pack.
- Protocol v1 uses H/T/P/F/A/C/E, at most 18,000 characters per complete message and a window of at
  most four chunks. Payload chunks default to 12,000 bytes, reduced for a smaller negotiated text
  limit. ACKs mean receiver-side writes, not Bluetooth/platform send completion.
- Each control request waits for the matching acknowledgement. Chunks have independent
  acknowledgement deadlines; only unconfirmed chunks are retried. Each request/chunk has at most
  four attempts, eight seconds per acknowledgement wait.
- A retry of the **same installation task** uses `resume`, the exact source fingerprint and the
  original chunk size, and honors `receivedRanges`. Starting a new task uses `replace`.
- A package is successful only after each file's C acknowledgement and the final `T finish` ready
  response.
- Activation remains a Manager operation. `active-theme` is an error: switch to another
  theme/default on-device before replacing the active package.

## Naming

`canopus-resource-pack` (format marker), `corona.json` (manifest file), `ng.lst.corona` /
`corona` (module package and runtime ids), `Corona` (authoring tool and project brand),
`.crpack` (container), `res_pack` (index type). The manifest filename was `canora.json` before the
Corona rename. The sender accepts the legacy `canora.json` name for compatibility and preserves
its original transfer path; both names must be at the archive root, and a pack containing both
is rejected as ambiguous. Legacy-name acceptance by the sender does not guarantee acceptance by
the device Manager. New packs must use `corona.json`. The format marker still reads `canopus-resource-pack` because it is a frozen wire constant that the receiver
matches literally, and changing it would require a `formatVersion` bump.

## Base91 interoperability

The variant is Joachim Henke's standard basE91. Each chunk is encoded independently with this
91-character alphabet:

```text
ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&()*+,./:;<=>?@[]^_`{|}~"
```

Test vectors: empty bytes encode to an empty string; UTF-8 `Hello World!` encodes to `>OwJh>Io0Tv!8PE`.