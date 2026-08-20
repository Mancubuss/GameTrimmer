// === SteamDB / Steam Free Game License Activator ===
// Open https://store.steampowered.com/account/licenses/ in your browser
// Press F12 -> Console -> Paste and run this script:

(async function addFreeGames() {
    const packages = [481512, 361730, 864508, 1004128, 182705, 760455, 454341, 760920, 919318, 765406, 200844, 231454, 28620, 26909, 574544, 890861, 785640, 472535, 727781, 1546, 232049, 757134, 2529, 29519, 7525, 28312, 494532, 169974, 244439, 754020, 78152, 17294, 105073, 78248, 317772, 894178, 301416, 46162, 42168, 447477, 400196, 554378, 396860, 981144, 563452, 1121087, 26615, 70808, 994640, 971360, 789940, 804791, 371727, 922110, 873830, 923230, 941420, 1004820, 1039860, 1043250, 504824, 470762, 50669];
    const apps = [1172470, 1085660, 2357570, 2767030, 578080, 2073850, 1240440, 2074920, 2507950, 2087030, 677620, 760160, 730, 570, 440, 238960, 230410, 1599340, 2429640, 2139460, 1286830, 1985790, 8500, 761890, 2064650, 9900, 109600, 24200, 236390, 1407200, 552990, 790710, 2055400, 386180, 212070, 444090, 386360, 961200, 1203220, 918450, 304930, 291550, 1222670, 203770, 1537830, 1184140, 2708450, 1568590, 3132990, 231350, 365670, 2738000, 2679460, 2154900, 2243880, 1105510, 2515080, 2384010, 2518420, 2577610, 2769490, 2898490, 2908860, 1433600, 1354890, 319630];
    console.log(`[GameTrimmer Test Setup] Starting activation of ${packages.length} packages...`);
    
    let successCount = 0;
    let failedCount = 0;
    for (let i = 0; i < packages.length; i++) {
        const subId = packages[i];
        try {
            const res = await fetch('/checkout/addfreelicense', {
                method: 'POST',
                headers: { 'Content-Type': 'application/x-www-form-urlencoded; charset=UTF-8' },
                body: new URLSearchParams({
                    action: 'add_to_cart',
                    sessionid: g_sessionID,
                    subid: subId
                })
            });
            console.log(`[+] [${i+1}/${packages.length}] Added package SubID: ${subId}`);
            successCount++;
            await new Promise(r => setTimeout(r, 450)); // anti-rate-limit delay
        } catch (err) {
            console.warn(`[-] Failed SubID ${subId}:`, err);
            failedCount++;
        }
    }
    console.log(`[GameTrimmer Test Setup] Done! Successfully registered ${successCount} packages (Failed: ${failedCount}).`);
    console.log(`To install games, use steam://install/<AppID> or launch the Steam Client.`);
})();
