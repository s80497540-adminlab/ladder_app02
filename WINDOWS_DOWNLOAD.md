# 📥 How to Download & Run Windows Build

## Download Location

Your Windows 11 binaries are ready in:

```
/workspaces/ladder_app02/dydx_numero/v4-clients/v4-client-rs/ladder_app02/releases/windows_20260120/
```

### Files Available for Download:

1. **ladder_app02.exe** (53 MB)
   - Main trading application with GUI
   - All order types supported (Market, Limit, Stop, Take-Profit)
   
2. **data_daemon02.exe** (14 MB)
   - Background service for live market data
   - Optional but recommended for instant UI hydration
   
3. **README.md** (4.3 KB)
   - Complete setup and usage instructions
   
4. **ladder_app02_windows_20260120.zip** (24 MB)
   - Complete package with all files (compressed)

## Download Method

You have several options:

### Option A: Download Entire Package (Easiest)
```
releases/ladder_app02_windows_20260120.zip (24 MB)
```
- Extract to any folder on your Windows 11 machine
- Both .exe files included
- README included

### Option B: Download Individual Files
```
releases/windows_20260120/ladder_app02.exe (53 MB)
releases/windows_20260120/data_daemon02.exe (14 MB)
releases/windows_20260120/README.md (4.3 KB)
```

### Option C: Clone/Pull from Git
```bash
git clone https://github.com/s80497540-adminlab/ladder_app02
cd ladder_app02/dydx_numero/v4-clients/v4-client-rs/ladder_app02/releases/windows_20260120/
# Files ready to use
```

## Setup Instructions (Windows 11)

### Step 1: Extract/Download
Choose one:
- Extract the ZIP file to a folder (e.g., `C:\Users\YourName\ladder_app`)
- Or download individual files to a folder

### Step 2: Run Data Daemon (Recommended)
```bash
data_daemon02.exe
```
- Keep this running in background
- It caches live order book and trade data
- Leave running while using the UI

### Step 3: Launch Application
```bash
ladder_app02.exe
```
- Main trading UI opens
- Falls back to dummy data if daemon not running

### Step 4: Connect Wallet
1. Click "Settings" button
2. Click "Connect Wallet"
3. Approve in Keplr browser extension
4. Create trading session (60+ seconds)
5. Toggle "REAL" mode to enable live trading

### Step 5: Place Orders
1. Select order type from dropdown
2. Fill in price/trigger if needed
3. Enter size and leverage
4. Click "Send Order"

## System Requirements Verification

Before running, verify:

```powershell
# Check Windows version (should be 11)
cmd /c "ver"

# Check disk space (need ~200 MB)
dir C:\ /L

# Test internet connectivity
ping dydx-ops-grpc.kingnodes.com
```

## Features Available

✅ **All 6 Order Types:**
- Market orders (instant execution)
- Limit orders (price-specific)
- Stop-Limit (conditional + limit)
- Stop-Market (conditional + market)
- Take-Profit Limit (conditional profit-taking)
- Take-Profit Market (conditional market exit)

✅ **Time-in-Force Options:**
- GTC (Good-Til-Cancel)
- IOC (Immediate-Or-Cancel)
- FOK (Fill-Or-Kill)
- POST_ONLY (Maker-only)

✅ **UI Features:**
- Real-time order book depth
- Live candlestick charts (multiple timeframes)
- Trading ladder with one-click buy/sell
- Account P&L tracking
- Open orders management
- Chart drawing tools with persistence

## Build Details

| Property | Value |
|----------|-------|
| **Build Date** | January 20, 2026 |
| **Version** | 0.3.0 |
| **Target OS** | Windows 11 x86_64 |
| **Architecture** | 64-bit Intel/AMD |
| **Cross-compiled from** | Linux (Ubuntu 24.04) |
| **Rust Toolchain** | 1.92.0 |
| **Status** | ✅ Tested & Ready |

## Troubleshooting

### "App won't start"
- Ensure Windows 11 (not Windows 10)
- Try running as Administrator
- Check Windows Defender isn't blocking it

### "No data displayed"
- Start `data_daemon02.exe` first
- Check internet connectivity
- Wait 10-20 seconds for data to load

### "Can't connect wallet"
- Ensure Keplr extension is installed
- Check dYdX chain is selected in Keplr
- Refresh page (F5) and retry

### "Order submission fails"
- Verify session is still active
- Check account has sufficient collateral
- Ensure REAL mode is armed (green status)

## Support Resources

- **README:** `releases/windows_20260120/README.md`
- **Documentation:** `ORDER_TYPES.md` (in project root)
- **GitHub:** dydxprotocol/v4-clients

## Next Steps

1. ✅ Download the files (ZIP or individual)
2. ✅ Extract to your machine
3. ✅ Run `data_daemon02.exe` (background)
4. ✅ Launch `ladder_app02.exe`
5. ✅ Connect wallet in Settings
6. ✅ Start trading!

---

**Ready to go! Happy trading! 📈**

Questions? Check the README.md file or GitHub issues.
