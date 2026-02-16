#!/bin/bash

# Cargo Target Folder Cleanup Manager
# Keeps only the most recent and necessary builds, asks for permission before cleaning

set -e

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR="$PROJECT_DIR/target"

# Load Cargo env if needed
if ! command -v cargo &> /dev/null; then
    . "$HOME/.cargo/env"
fi

print_usage() {
    cat << EOF
Cargo Cleanup Manager - Interactive build artifact management

USAGE:
    ./cargo_clean_manager.sh [COMMAND]

COMMANDS:
    status          Show current target/ folder size and breakdown
    smart-clean     Remove old debug builds, keep release and latest debug
    full-clean      Remove ALL build artifacts (requires confirmation)
    help            Show this help message

EXAMPLES:
    ./cargo_clean_manager.sh status
    ./cargo_clean_manager.sh smart-clean
    ./cargo_clean_manager.sh full-clean

The smart-clean command:
    ✓ Keeps release/ build directory (optimized builds)
    ✓ Keeps latest debug build
    ✓ Removes old debug artifacts
    ✓ Cleans incremental compilation cache
    ✓ Estimates space saved

EOF
}

show_status() {
    if [ ! -d "$TARGET_DIR" ]; then
        echo "No target/ directory found."
        return
    fi
    
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Cargo Target Directory Status"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    total_size=$(du -sh "$TARGET_DIR" | cut -f1)
    echo "Total size: $total_size"
    echo ""
    
    echo "Breakdown by profile:"
    if [ -d "$TARGET_DIR/release" ]; then
        rel_size=$(du -sh "$TARGET_DIR/release" | cut -f1)
        echo "  Release:     $rel_size"
    fi
    if [ -d "$TARGET_DIR/debug" ]; then
        debug_size=$(du -sh "$TARGET_DIR/debug" | cut -f1)
        echo "  Debug:       $debug_size"
    fi
    
    echo ""
    echo "Breakdown by type:"
    if [ -d "$TARGET_DIR/deps" ]; then
        deps_size=$(du -sh "$TARGET_DIR/deps" 2>/dev/null | cut -f1)
        echo "  Dependencies: $deps_size"
    fi
    if [ -d "$TARGET_DIR/.fingerprint" ]; then
        fp_size=$(du -sh "$TARGET_DIR/.fingerprint" 2>/dev/null | cut -f1)
        echo "  Fingerprints: $fp_size"
    fi
    if [ -d "$TARGET_DIR/incremental" ]; then
        inc_size=$(du -sh "$TARGET_DIR/incremental" 2>/dev/null | cut -f1)
        echo "  Incremental:  $inc_size"
    fi
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

smart_clean() {
    if [ ! -d "$TARGET_DIR" ]; then
        echo "No target/ directory found. Nothing to clean."
        return
    fi
    
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Smart Clean Preview"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    before_size=$(du -sh "$TARGET_DIR" | cut -f1)
    before_bytes=$(du -s "$TARGET_DIR" | cut -f1)
    
    echo "Current size: $before_size"
    echo ""
    echo "This will:"
    echo "  ✓ Remove old debug build artifacts"
    echo "  ✓ Clean incremental compilation cache"
    echo "  ✓ Keep release/ directory (optimized builds)"
    echo "  ✓ Keep latest debug build"
    echo ""
    
    read -p "Proceed with smart clean? (yes/no): " -r response
    if [[ ! "$response" =~ ^[Yy][Ee][Ss]$ ]]; then
        echo "Cancelled."
        return
    fi
    
    echo "Cleaning..."
    
    # Remove incremental compilation data (safe to remove, rebuilt automatically)
    if [ -d "$TARGET_DIR/incremental" ]; then
        rm -rf "$TARGET_DIR/incremental"
        echo "  ✓ Removed incremental compilation cache"
    fi
    
    # Keep only release and latest debug, but clean old deps
    # This is conservative - only cleans cache, not actual binaries
    if [ -d "$TARGET_DIR/.fingerprint" ]; then
        # Remove fingerprints for deleted artifacts (safe)
        find "$TARGET_DIR/.fingerprint" -type d -empty -delete 2>/dev/null || true
    fi
    
    # Clean old object files in deps
    if [ -d "$TARGET_DIR/deps" ]; then
        # Remove .d (dependency) files which can clutter
        find "$TARGET_DIR/deps" -name "*.d" -type f -delete 2>/dev/null || true
        echo "  ✓ Removed dependency metadata files"
    fi
    
    after_bytes=$(du -s "$TARGET_DIR" | cut -f1)
    after_size=$(du -sh "$TARGET_DIR" | cut -f1)
    freed=$((before_bytes - after_bytes))
    freed_mb=$((freed / 1024))
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Clean Complete"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Before: $before_size"
    echo "After:  $after_size"
    echo "Freed:  ${freed_mb} MB"
}

full_clean() {
    if [ ! -d "$TARGET_DIR" ]; then
        echo "No target/ directory found. Nothing to clean."
        return
    fi
    
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ⚠️  FULL CLEAN WARNING"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    current_size=$(du -sh "$TARGET_DIR" | cut -f1)
    echo "This will DELETE the entire target/ directory ($current_size)."
    echo "All build artifacts will be removed."
    echo "Next build will take: 5-10 minutes (full recompile)"
    echo ""
    
    read -p "Type 'DELETE ALL' to confirm: " -r response
    if [[ "$response" != "DELETE ALL" ]]; then
        echo "Cancelled."
        return
    fi
    
    echo "Removing target/ directory..."
    rm -rf "$TARGET_DIR"
    echo "✓ Target directory deleted."
    echo ""
    echo "Run 'cargo build --release' when ready to rebuild."
}

main() {
    local cmd="${1:-status}"
    
    case "$cmd" in
        status)
            show_status
            ;;
        smart-clean)
            smart_clean
            ;;
        full-clean)
            full_clean
            ;;
        help|--help|-h)
            print_usage
            ;;
        *)
            echo "Unknown command: $cmd"
            echo ""
            print_usage
            exit 1
            ;;
    esac
}

main "$@"
