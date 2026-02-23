# macOS Build Instructions for ladder_app02

## Build on Your MacBook Pro (Recommended)

Since you're on a MacBook Pro with Sonoma, the easiest and most reliable way to get a working binary is to build it locally.

### Prerequisites

1. Install Rust (if not already installed):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

2. Install Homebrew (if not already installed):
```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

3. Install required dependencies:
```bash
brew install cmake pkg-config openssl
```

### Build Steps

1. Clone or pull the latest code from your repository:
```bash
cd ~/Documents  # or wherever you want to build
git clone https://github.com/s80497540-adminlab/ladder_app02.git
cd ladder_app02
git checkout optimization_two
```

2. Navigate to the project directory:
```bash
cd dydx_numero/v4-clients/v4-client-rs/ladder_app02
```

3. Build the release binary:
```bash
cargo build --release
```

This will take 5-15 minutes depending on your machine.

4. The binary will be created at:
```bash
./target/release/ladder_app02
```

5. Run the application:
```bash
./target/release/ladder_app02
```

### Troubleshooting

If you get a security warning when running the app:
- Right-click the binary and select "Open"
- Or go to System Settings → Privacy & Security and click "Open Anyway"

If you get build errors related to OpenSSL:
```bash
brew reinstall openssl
export PKG_CONFIG_PATH="/opt/homebrew/opt/openssl@3/lib/pkgconfig"
cargo clean
cargo build --release
```

### File Size
The release binary should be approximately 36-40MB.
