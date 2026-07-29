# Globustut uploader to Commons

Rust service for accepting freely licensed media from [Глобус Беларусі](https://globustut.by/), holding it in a private moderation queue, and publishing approved files to Wikimedia Commons.

## Included in the MVP

- public multi-file intake with a required Commons category and a fixed CC BY 4.0 license;
- content-based media detection instead of trusting browser MIME types or extensions;
- unchanged storage for Commons-compatible JPEG, PNG, GIF, TIFF, and WebP files;
- conversion of other images understood by libvips or ImageMagick to WebP;
- DNG handling that extracts the largest decoder-valid JPEG preview exposed by ExifTool first, then develops the raw image with LibRaw and encodes it as WebP (with libvips/ImageMagick as a final fallback);
- acceptance of already compatible WebM, Ogg Theora, and MPEG-1/2 video, with no video conversion;
- EXIF/XMP author, capture date, and GPS extraction, with an OpenStreetMap link in moderation;
- local SQLite moderator accounts using Argon2id passwords and private sessions;
- rename, multi-category editing with automatic persistence, and permanent deletion controls;
- Commons category autocomplete plus up to 20 closest categories from Wikidata `P625` and `P373`;
- sequential publication to Commons through the single `Globustut` account, with a direct link returned for every published file and local cleanup after success;
- full-viewport image previews on mouse hover in moderation;
- responsive light/dark UI, with an exact `#000` dark page background;
- Belarusian-only user interface;
- unit, media integration, HTTP workflow, and Playwright desktop/mobile tests.

There is deliberately no OAuth code. Local login protects only `/admin` and is unrelated to the Commons account. Publishing uses the dedicated BotPassword login `Globustut@globustut-uploader`; the password lives in a mode-`0600` file under the private Toolforge data directory and is never stored in SQLite, committed, sent to the browser, or logged.

## Image normalization

Here, normalization means identifying the real image format, applying its orientation while converting, writing unsupported formats as Commons-compatible WebP, and generating a small moderation thumbnail. Already supported formats are preserved byte-for-byte, so JPEG photos are not needlessly recompressed. For DNG, ExifTool identifies the format and extracts TIFF/DNG-referenced JPEG previews without scanning arbitrary raw bytes; the largest decoder-valid preview is preserved byte-for-byte when available. Otherwise the raw image is developed and encoded as WebP.

The file chooser permits every extension because the server performs the actual validation. "Any format" means any still-image format decoded by the installed libvips/ImageMagick stack and within the configured size/pixel limits. Damaged files, documents, executables, and unsupported video codecs are rejected.

## Local development

Requirements:

- current stable Rust;
- Node.js 22 or newer for browser tests;
- `vips`, ImageMagick, FFmpeg/ffprobe, and ExifTool;
- optional RAW/HEIF decoders (`dcraw`/`libraw` and `libheif`).

```bash
cp .env.example .env
set -a
source .env
set +a
cargo run -- user add moderator
cargo run -- serve
```

Open `http://127.0.0.1:8000/` for upload and `http://127.0.0.1:8000/admin` for moderation. The source site can prefill a category:

```text
https://globustut-uploader-to-commons.toolforge.org/?category=Castles_in_Belarus
```

Direct multipart clients submit repeated `files` fields plus one `category` field. They do not submit a license or rights-confirmation field: the server records every accepted upload as CC BY 4.0.

Account commands operate on the configured SQLite database:

```bash
cargo run -- user list
cargo run -- user add another-moderator
cargo run -- user delete another-moderator
```

Passwords must contain at least 12 characters. For non-interactive setup, put a password in a short-lived environment variable and use `user add NAME --password-env VARIABLE`.

Configure the fixed Commons publishing account with a dedicated
[BotPassword](https://commons.wikimedia.org/wiki/Special:BotPasswords) named
`globustut-uploader`. It needs the basic, upload, and page-editing grants. The
interactive command writes only the password:

```bash
cargo run -- commons set-password
cargo run -- commons status
```

### Changing the Commons account or password

The publishing account is intentionally fixed at compile time. To use another
Wikimedia account, edit both constants at the top of
[`src/publisher.rs`](src/publisher.rs):

```rust
pub const COMMONS_ACCOUNT: &str = "NewAccount";
pub const COMMONS_BOT_USERNAME: &str = "NewAccount@globustut-uploader";
```

`COMMONS_ACCOUNT` is the Wikimedia username shown in file links and used to
verify completed uploads. `COMMONS_BOT_USERNAME` is the full BotPassword login
in `Account@BotPasswordName` form. Create that BotPassword under the new account
with the basic, upload, and page-editing grants, then commit and redeploy the
code.

To replace only the BotPassword on Toolforge, do not edit the repository or
paste the password into a command:

```bash
ssh login.toolforge.org
become globustut-uploader-to-commons
toolforge webservice shell
cd /workspace
./scripts/run-toolforge.sh commons set-password
exit
toolforge webservice restart
```

The command prompts twice and replaces the private mode-`0600` password file.
`commons status` confirms that the file exists; the credentials are validated
against Commons when the next publication starts.

Restart the server after changing the password. The browser sends the files
visible on the current moderation page one at a time and appends their Commons
links as each upload completes, so keep the page open until the batch finishes.
A successfully published file is removed from the local queue; failed files
remain available for retry.

## Configuration

The defaults are shown in [.env.example](.env.example). Important settings are:

| Variable | Default | Purpose |
| --- | --- | --- |
| `TOOL_DATA_DIR` | `data`, or a private child of Toolforge NFS | SQLite, pending originals, and thumbnails |
| `APP_TEMP_DIR` | OS temp directory plus `globustut-uploader` | upload buffering and conversions |
| `MAX_FILE_MB` | `200` | per-file input limit |
| `MAX_REQUEST_MB` | `2048` | combined file payload limit per submission |
| `MAX_FILES` | `500` | files per submission |
| `MAX_PENDING_MB` | `20000` | total indexed moderation queue limit |
| `DISK_RESERVE_MB` | `512` | required free filesystem reserve |
| `MAX_PIXELS` | `250000000` | image decompression safety limit |
| `REQUEST_TIMEOUT_SECONDS` | `3600` | complete request deadline |
| `SESSION_TTL_HOURS` | `12` | moderator session lifetime |
| `COMMONS_API_TIMEOUT_SECONDS` | `300` | timeout for one Commons API request |
| `COMMONS_BOT_PASSWORD_FILE` | private data directory | dedicated BotPassword file |
| `NEARBY_RADIUS_METERS` | `10000` | Wikidata geographic search radius |
| `NEARBY_CATEGORY_LIMIT` | `20` | result limit, always clamped to 20 |
| `PUBLIC_BASE_URL` | inferred | public URL and secure-cookie default |

The per-file, file-count, and submission limits are independent. A batch may contain up to 500 files only when their combined file payload remains within 2 GiB; the HTTP body limit includes a small additional allowance for multipart headers.

SQLite uses one connection per blocking operation, full synchronous writes, and rollback journaling. The Toolforge service therefore runs exactly one replica. This is suitable for a low-volume moderation queue; move the database to ToolsDB before scaling replicas or write volume. The admin disk counter shows indexed queue bytes against `MAX_PENDING_MB`; the separate free-space check still uses the underlying filesystem.

Startup reconciliation removes unindexed files left by a prior crash. App-owned directories and the database use private filesystem permissions. Back up the SQLite file and `pending/` together while the webservice is stopped for a consistent manual snapshot.

Category syntax is checked against the main MediaWiki title restrictions, but existence is not required because a Commons outage must not block intake. The autocomplete and category links let the moderator verify or create an intentional red-link category later.

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
npm ci
npx playwright install chromium
npm run test:e2e
```

The same checks run in [GitHub Actions](.github/workflows/ci.yml). Browser tests use a local Wikimedia API fixture, so sorting, the 20-category limit, autocomplete, and no-EXIF behavior are deterministic.

## Toolforge deployment

The Build Service template requests the largest normal web pod available to tools: 3 CPU and 6 GiB RAM. `mount: all` exposes persistent NFS, while conversion work uses the pod's larger ephemeral `/tmp`. Toolforge does not expose a per-service "largest storage" field; storage capacity and quotas are platform-managed.

Deploy directly from this workstation:

```bash
./scripts/toolforge-deploy-local.sh
```

Or run the underlying script on a Toolforge bastion:

```bash
ssh login.toolforge.org
become globustut-uploader-to-commons
git clone https://github.com/vitaly-zdanevich/globustut-uploader-to-commons.git source
cd source
./scripts/toolforge-deploy.sh
```

Both paths use the public repository as Build Service input, install `service.template`, build, start or restart the service, and wait for `/healthz`. The local wrapper only performs a fast-forward pull; it does not overwrite remote changes.

Create the first local moderator and configure the Commons BotPassword inside
the built image:

```bash
toolforge webservice shell
cd /workspace
./scripts/run-toolforge.sh user add moderator
./scripts/run-toolforge.sh commons set-password
exit
```

Operational commands:

```bash
./scripts/toolforge-logs.sh
toolforge webservice status
toolforge webservice shell
```

Persistent state is kept under `/data/project/TOOL_NAME/globustut-uploader`. The runtime wrapper uses Toolforge's injected `TOOL_DATA_DIR`, and the app derives the tool name from that path so moderator cookies are `Secure` in production.

## Deployment policy boundary

Resolve these points with Toolforge administrators before enabling the public form:

1. The current intake route is anonymous because that is the requested product flow. Toolforge's rules generally require users who publish content through a tool to be registered and validated. Confirm that private, premoderated staging qualifies, or add an accepted uploader-authentication mechanism. The local moderator password is not uploader validation.
2. Pending images and EXIF GPS are private user data stored outside Commons. Publish a retention/privacy policy, limit moderator access, and delete rejected media promptly.
3. The queue has a byte cap and disk reserve, but public intake still needs operational monitoring and a retention decision before launch.
4. The server assigns CC BY 4.0 to every upload; moderation must still check authorship, permission, privacy, Commons scope, and filename/category collisions before publication.

Relevant platform documentation: [Toolforge Build Service](https://wikitech.wikimedia.org/wiki/Help%3AToolforge/Build_service), [webservices](https://wikitech.wikimedia.org/wiki/Help%3AToolforge/Web), [Kubernetes storage](https://wikitech.wikimedia.org/wiki/Help%3AToolforge/Kubernetes), [Toolforge rules](https://wikitech.wikimedia.org/wiki/Help%3AToolforge/Rules), and [Wikimedia Cloud Services terms](https://wikitech.wikimedia.org/wiki/Cloud_Services_Terms_of_use).

## License

[MIT](LICENSE)
