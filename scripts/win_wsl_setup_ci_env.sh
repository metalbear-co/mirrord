#!/bin/bash
# mirrord CI Environment Setup Script for WSL
# This script replicates the exact environment used in GitHub CI for integration tests

set -e

echo "🚀 Setting up mirrord CI environment on WSL..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

print_step() {
    echo -e "${GREEN}▶${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

# Check if running on WSL
if ! grep -qi microsoft /proc/version; then
    print_error "This script is designed for WSL. Please run it from Windows Subsystem for Linux."
    exit 1
fi

print_step "Updating system packages..."
sudo apt update && sudo apt upgrade -y

print_step "Installing essential build tools..."
sudo apt install -y \
    build-essential \
    curl \
    wget \
    git \
    pkg-config \
    libssl-dev \
    ca-certificates \
    gnupg \
    lsb-release \
    protobuf-compiler \
    unzip \
    zip

print_step "Installing Clang and LLVM (required for bindgen/frida-gum-sys)..."
sudo apt install -y \
    clang \
    libclang-dev \
    llvm-dev \
    libllvm18 \
    libclang1-18 \
    libclang-18-dev \
    libclang-common-18-dev \
    libclang-cpp18 \
    libclang-rt-18-dev

print_step "Installing additional development libraries..."
sudo apt install -y \
    libffi-dev \
    libxml2-dev \
    libncurses-dev \
    libicu-dev \
    zlib1g-dev \
    libgc1

# Install Rust (matching CI setup)
print_step "Installing Rust toolchain..."
if ! command -v rustup &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source ~/.cargo/env
else
    print_warning "Rust already installed, updating..."
    rustup update
fi

# Add required Rust components
rustup component add clippy rustfmt
rustup target add x86_64-unknown-linux-gnu

# Install Java (matching CI: OpenJDK 17.0.6-tem via SDKMAN)
print_step "Installing Java via SDKMAN..."
if [ ! -d "$HOME/.sdkman" ]; then
    # Ensure unzip and zip are available for SDKMAN
    if ! command -v unzip &> /dev/null || ! command -v zip &> /dev/null; then
        print_error "unzip and zip are required for SDKMAN but not installed"
        exit 1
    fi
    curl -s "https://get.sdkman.io" | bash
    source "$HOME/.sdkman/bin/sdkman-init.sh"
    sdk install java 17.0.6-tem
else
    print_warning "SDKMAN already installed"
    source "$HOME/.sdkman/bin/sdkman-init.sh"
    if ! sdk list java | grep -q "17.0.6-tem"; then
        sdk install java 17.0.6-tem
    fi
fi

# Install Node.js (matching CI: Node 18)
print_step "Installing Node.js v18..."
if ! command -v node &> /dev/null || [ "$(node -v | cut -d'.' -f1 | sed 's/v//')" != "18" ]; then
    curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
    sudo apt-get install -y nodejs
else
    print_warning "Node.js v18 already installed"
fi

# Install Node.js dependencies (matching CI)
print_step "Installing Node.js test dependencies..."
npm install express@4.21.2

# Install Python and dependencies (matching CI)
print_step "Installing Python and test dependencies..."
sudo apt install -y python3 python3-pip python3-dev python3-venv
pip3 install --break-system-packages flask fastapi uvicorn[standard]

# Install Go versions (matching CI: 1.23, 1.24, 1.25)
print_step "Installing Go versions (1.23, 1.24, 1.25)..."

# Function to install Go version
install_go_version() {
    local version=$1
    local go_dir="/usr/local/go${version}"
    
    if [ ! -d "$go_dir" ]; then
        print_step "Installing Go $version..."
        wget -q "https://go.dev/dl/go${version}.linux-amd64.tar.gz"
        sudo tar -C /usr/local -xzf "go${version}.linux-amd64.tar.gz"
        sudo mv /usr/local/go "$go_dir"
        rm "go${version}.linux-amd64.tar.gz"
    else
        print_warning "Go $version already installed"
    fi
}

# Install Go versions
install_go_version "1.23.12"
install_go_version "1.24.7"
install_go_version "1.25.1"

# Create Go version switcher script
print_step "Creating Go version switcher..."
cat > ~/.go-version << 'EOF'
#!/bin/bash
# Go version switcher for mirrord development

switch_go() {
    local version=$1
    local go_dir="/usr/local/go${version}"
    
    if [ ! -d "$go_dir" ]; then
        echo "Go version $version not installed"
        return 1
    fi
    
    # Remove any existing Go from PATH
    export PATH=$(echo $PATH | sed 's|/usr/local/go[^:]*:||g')
    
    # Add the requested Go version to PATH
    export PATH="$go_dir/bin:$PATH"
    
    echo "Switched to Go $version"
    go version
}

# Default to Go 1.23
switch_go "1.23.12"
# Aliases for easy switching
alias go23="switch_go 1.23.12"
alias go24="switch_go 1.24.7"
alias go25="switch_go 1.25.1"
EOF

# Add Go switcher to bashrc
if ! grep -q "source ~/.go-version" ~/.bashrc; then
    echo "source ~/.go-version" >> ~/.bashrc
fi

# Source it for current session
source ~/.go-version

print_step "Setting up environment variables for testing..."
cat >> ~/.bashrc << 'EOF'

# mirrord testing environment variables
export MIRRORD_TELEMETRY=false
export RUST_LOG=mirrord=debug
export CARGO_NET_GIT_FETCH_WITH_CLI=true

# Helper function to set mirrord test env vars
mirrord_test_env() {
    export MIRRORD_FILE_MODE=localwithoverrides
    export MIRRORD_FILE_READ_WRITE_PATTERN=/app/test.txt
    echo "Set mirrord test environment variables"
}
EOF

print_step "Building test applications (matching CI process)..."

# Navigate to mirrord directory (assuming script is run from repo root)
if [ ! -f "Cargo.toml" ] || ! grep -q "mirrord" Cargo.toml; then
    print_error "Please run this script from the mirrord repository root directory"
    exit 1
fi

# Build Rust test apps (matching CI)
rust_test_apps=(
    "mirrord/layer/tests/apps/issue1123"
    "mirrord/layer/tests/apps/issue1054" 
    "mirrord/layer/tests/apps/issue1458"
    "mirrord/layer/tests/apps/issue1458portnot53"
    "mirrord/layer/tests/apps/issue2058"
    "mirrord/layer/tests/apps/issue2204"
    "mirrord/layer/tests/apps/fileops"
    "mirrord/layer/tests/apps/outgoing"
    "mirrord/layer/tests/apps/recv_from"
    "mirrord/layer/tests/apps/dns_resolve"
    "mirrord/layer/tests/apps/listen_ports"
    "mirrord/layer/tests/apps/issue1776"
    "mirrord/layer/tests/apps/issue1776portnot53"
    "mirrord/layer/tests/apps/issue1899"
    "mirrord/layer/tests/apps/issue2001"
    "mirrord/layer/tests/apps/issue2438" 
    "mirrord/layer/tests/apps/issue3248"
    "mirrord/layer/tests/apps/rebind0"
)

# Build simple Rust apps
for app in "${rust_test_apps[@]:0:6}"; do
    if [ -d "$app" ]; then
        print_step "Building Rust test app: $app"
        cd "$app"
        mkdir -p target
        rustc *.rs --out-dir target
        cd - > /dev/null
    fi
done

# Build Cargo-based Rust apps
for app in "${rust_test_apps[@]:6}"; do
    if [ -d "$app" ]; then
        print_step "Building Cargo test app: $app"
        cd "$app"
        cargo build
        cd - > /dev/null
    fi
done

# Build C apps
print_step "Building C test applications..."
if [ -f "scripts/build_c_apps.sh" ]; then
    chmod +x scripts/build_c_apps.sh
    ./scripts/build_c_apps.sh
fi

# Build Go apps for all versions (matching CI)
print_step "Building Go test applications..."
if [ -f "scripts/build_go_apps.sh" ]; then
    chmod +x scripts/build_go_apps.sh
    
    cd mirrord/layer/tests
    
    # Source the Go version script to get aliases
    source ~/.go-version
    
    # Build with Go 1.23
    switch_go "1.23.12"
    ../../../scripts/build_go_apps.sh 23
    
    # Build with Go 1.24
    switch_go "1.24.7"
    ../../../scripts/build_go_apps.sh 24
    
    # Build with Go 1.25
    switch_go "1.25.1"
    ../../../scripts/build_go_apps.sh 25
    
    cd - > /dev/null
fi

print_step "Testing the setup..."

# Test Rust compilation
print_step "Testing Rust compilation..."
cargo check -p mirrord-layer

# Test clippy
print_step "Testing clippy..."
cargo clippy -p mirrord-layer -- -D warnings

# Test basic Go compilation
print_step "Testing Go compilation..."
source ~/.go-version
switch_go "1.23.12"
go version

print_step "✅ CI environment setup complete!"
echo ""
echo "🔧 Available commands:"
echo "  go23, go24, go25  - Switch between Go versions"
echo "  mirrord_test_env   - Set mirrord test environment variables"
echo ""
echo "🧪 Run integration tests:"
echo "  cd /path/to/mirrord"
echo "  cargo test --target x86_64-unknown-linux-gnu -p mirrord-layer"
echo ""
echo "🐛 Run specific failing tests:"
echo "  cargo test --target x86_64-unknown-linux-gnu -p mirrord-layer faccessat_go"
echo "  cargo test --target x86_64-unknown-linux-gnu -p mirrord-layer read_go"
echo "  cargo test --target x86_64-unknown-linux-gnu -p mirrord-layer write_go"
echo ""
echo "📝 Remember to source your bashrc or restart your shell:"
echo "  source ~/.bashrc"
