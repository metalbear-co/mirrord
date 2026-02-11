# xtask - mirrord Build Automation

This is mirrord's build automation tool, following the Rust [`xtask` pattern](https://github.com/matklad/cargo-xtask).

## Quick Start

Build release CLI for your platform:

```bash
cargo xtask build-cli --release
```

## Commands

### `build-cli`

Builds the complete CLI with all dependencies. This is the main command that orchestrates:
1. Building the wizard frontend (npm install + build)
2. Building the mirrord layer for the target platform
3. Building the mirrord CLI binary
4. Creating universal binaries (on macOS)
5. Signing binaries (on macOS)

**Usage:**

```bash
# Debug build for current platform (auto-detected)
cargo xtask build-cli

# Release build
cargo xtask build-cli --release

# Build for specific platform
cargo xtask build-cli --release --platform macos-universal
cargo xtask build-cli --release --platform linux-x86_64
cargo xtask build-cli --release --platform linux-aarch64
cargo xtask build-cli --release --platform windows

# Build without wizard frontend
cargo xtask build-cli --release --no-wizard
```

**Supported platforms:**
- `macos-universal` (or `macos`, `darwin`) - macOS universal binary (x86_64 + aarch64)
- `linux-x86_64` (or `linux-amd64`) - Linux x86_64
- `linux-aarch64` (or `linux-arm64`) - Linux ARM64
- `windows` (or `win`) - Windows x86_64

### `build-wizard`

Builds only the wizard frontend.

```bash
cargo xtask build-wizard
```

### `build-layer`

Builds only the mirrord layer.

```bash
# Debug build for current platform
cargo xtask build-layer

# Release build for specific platform
cargo xtask build-layer --platform macos-universal --release
cargo xtask build-layer --platform linux-x86_64 --release
```

## Examples

### Local Development on macOS

```bash
# Quick debug build for testing
cargo xtask build-cli

# Full release build
cargo xtask build-cli --release
```

### Building Linux Release on CI

```bash
# Linux x86_64
cargo xtask build-cli --release --platform linux-x86_64

# Linux ARM64 (requires cross-compilation setup)
cargo xtask build-cli --release --platform linux-aarch64
```

### Building Components Separately

```bash
# Build wizard only
cargo xtask build-wizard

# Build layer only
cargo xtask build-layer --release

# Build full CLI (builds wizard + layer + CLI)
cargo xtask build-cli --release
```

## Architecture

The xtask is organized into modules:

- `tasks/wizard.rs` - Builds the wizard frontend (npm install + build)
- `tasks/layer.rs` - Builds mirrord-layer for various platforms
- `tasks/cli.rs` - Builds mirrord CLI binary
- `tasks/release.rs` - Orchestrates the full build process

Each task can be run independently or composed together.

## macOS Universal Binaries

On macOS, the build process:
1. Builds layer for both x86_64 and aarch64
2. Builds an arm64e shim (for SIP bypass)
3. Signs architecture-specific binaries
4. Combines them into a universal dylib using `lipo`
5. Builds CLI for both architectures
6. Signs architecture-specific CLIs
7. Combines into a universal binary
8. Signs universal binaries

### Signing Behavior

The xtask automatically detects the environment and uses the appropriate signing method:

**CI Environment** (when `AC_USERNAME` and `AC_PASSWORD` are set):
- Uses `gon` with Apple Developer credentials
- Proper code signing and notarization
- Matches the behavior in `.github/workflows/release.yaml`

**Local Development** (when credentials are not set):
- Uses `codesign -f -s -` for ad-hoc signing
- Fast and suitable for local testing
- No Apple Developer account needed

**Note**: For CI signing, `gon` must be installed:
```bash
brew tap mitchellh/gon
brew install mitchellh/gon/gon
```

## CI/CD Integration

This xtask workflow is designed to replace the manual build steps in `.github/workflows/release.yaml` and `./scripts/build_fat_mac.sh`. The main advantages:

- **Type-safe**: Rust compiler ensures correctness
- **Composable**: Tasks can be run independently or orchestrated
- **Cross-platform**: Same tool works on all platforms
- **Maintainable**: All build logic in one place
- **Reusable**: Can be called from CI or run locally

## Comparison with Current Scripts

### Before (shell scripts)

```bash
./scripts/build_fat_mac.sh --release --features wizard
```

### After (xtask)

```bash
cargo xtask build-cli --release
```

Both do the same thing, but xtask:
- Provides better error messages
- Has consistent behavior across platforms
- Is easier to extend with new tasks
- Can be tested like any Rust code
