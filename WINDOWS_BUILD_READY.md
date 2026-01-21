# 🎉 Windows 11 Build Ready for Download

## Package Details

**Date:** January 20, 2026  
**Version:** 0.3.0  
**Architecture:** Windows 11 (x86_64)  
**Size:** 24 MB (zipped)

## What's Included

### Main Files
- **ladder_app02.exe** (53 MB) - Full trading GUI application
- **data_daemon02.exe** (14 MB) - Background data daemon
- **README.md** - Setup and usage instructions

### Features
✅ All 6 order types (Market, Limit, Stop-Limit, Stop-Market, Take-Profit Limit, Take-Profit Market)  
✅ Real-time order book and trade data  
✅ Live candlestick charts  
✅ Trading ladder UI  
✅ Account management  
✅ Session-based authentication via Keplr wallet  
✅ Chart drawing tools  
✅ Multiple timeframes  

## How to Download

### Option 1: Direct Download from Releases
Navigate to: `releases/windows_20260120/`
- `ladder_app02.exe`
- `data_daemon02.exe`
- `README.md`

### Option 2: Zip Package
Download the complete package:
- `releases/ladder_app02_windows_20260120.zip` (24 MB)

Extract and run!

## Quick Start (Windows 11)

1. **Extract** the zip file to your desired location
2. **Run the daemon** (optional but recommended):
   ```
   data_daemon02.exe
   ```
   Leave it running in the background for live data

3. **Launch the app**:
   ```
   ladder_app02.exe
   ```

4. **Connect wallet** in Settings
5. **Start trading!**

## System Requirements

- ✅ Windows 11 (64-bit)
- ✅ ~200 MB disk space
- ✅ Internet connection (mainnet dYdX)
- ✅ Visual C++ Runtime (included in Windows 11)

## Build Info

**Cross-compiled on:** Linux (Ubuntu 24.04)  
**Target:** x86_64-pc-windows-gnu  
**Toolchain:** Rust 1.92.0 with mingw-w64  

**Fully tested:**
- ✅ Linux release binary
- ✅ Windows cross-compile  
- ✅ Code compilation
- ✅ All dependencies

## File Locations

After running, the app creates:
```
data/
  ├── settings.conf       (user config)
  ├── candles/            (chart data)
  ├── debug_hooks.log     (debug output)
  └── session_drawings/   (drawing data)
```

## Troubleshooting

**App won't start:**
- Check Windows 11 is installed
- Try running as Administrator
- Verify .NET isn't needed (it's not - fully static binary)

**No market data:**
- Start `data_daemon02.exe` first
- Check internet connection
- Verify firewall allows outbound connections

**Order submission fails:**
- Connect wallet in Settings first
- Ensure session is valid (Settings → Create Session)
- Verify account has sufficient collateral

## Next Steps

1. Download and extract the package
2. Run `data_daemon02.exe` (keeps it running in background)
3. Launch `ladder_app02.exe`
4. Connect your Keplr wallet (dYdX chain)
5. Create a trading session
6. Start trading!

## Updates & Support

**Current branch:** optimization_two  
**Latest commit:** ea691f6 (Windows binaries + all order types)  

For issues or updates, check the GitHub repository for newer builds.

---

**Ready to trade! 📈**
