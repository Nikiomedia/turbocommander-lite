# Erzeugt die Bildschirmfotos für die README von TurboCommander Lite (deutsch und englisch) in docs\screenshots.
#
# Voraussetzungen: `cargo build --release` ist gelaufen, ffmpeg liegt im Suchpfad, der Laufwerksbuchstabe
# unten ist frei. Die Beispieldateien entstehen in target\demo; für saubere Pfade in den Bildern werden sie
# vorübergehend als eigenes Laufwerk eingebunden (subst) und danach wieder gelöst.
#
# TurboCommander Lite läuft dabei im Bildschirmfoto-Modus (--shot): eigene Standard-Einstellungen, es wird nichts
# gespeichert, der Ton ist aus.
param([string] $Drive = 'T:')

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = "$root\target\release\TurboCommanderLite.exe"
$demo = "$root\target\demo"
$art = "$demo\_quellen"
$out = "$root\docs\screenshots"
if (-not (Test-Path $exe)) { throw "Erst 'cargo build --release' ausführen." }
if (Test-Path $demo) { Remove-Item $demo -Recurse -Force }
New-Item -ItemType Directory -Force $art, "$out\de", "$out\en" | Out-Null

# ---------------------------------------------------------------- Beispielbilder: gezeichnete Landschaften
function Scene($top, $hor, $sun, $h1, $h2, $h3, $sx, $sy, $ph, $move) {
    $ch = @()
    for ($c = 0; $c -lt 3; $c++) {
        $sky = "($($top[$c])+($($hor[$c])-$($top[$c]))*min(1,Y/(H*0.72)))"
        $m = if ($move) { "+T*$move" } else { "" }
        $l1 = "H*(0.60+0.05*sin(X/260+$($ph[0])$m*0.4)+0.03*sin(X/97+$($ph[1])))"
        $l2 = "H*(0.72+0.06*sin(X/330+$($ph[2])$m*0.7)+0.02*sin(X/61+1))"
        $l3 = "H*(0.85+0.04*sin(X/180+$($ph[3])$m)+0.015*sin(X/43))"
        $glow = "max(0,1-hypot(X-W*$sx,Y-H*$sy)/(H*0.45))"
        $ch += "if(gt(Y,$l3),$($h3[$c]),if(gt(Y,$l2),$($h2[$c]),if(gt(Y,$l1),$($h1[$c]),if(lt(hypot(X-W*$sx,Y-H*$sy),H*0.07),$($sun[$c]),min(255,$sky+($($sun[$c])-$sky)*0.35*$glow*$glow)))))"
    }
    "geq=r='$($ch[0])':g='$($ch[1])':b='$($ch[2])'"
}
$sunset = @(@(25, 25, 70), @(255, 140, 80), @(255, 220, 150), @(120, 60, 90), @(70, 35, 70), @(30, 20, 45), 0.68, 0.47, @(0.4, 1.2, 2.0, 0.7))
$scenes = @(
    $sunset,
    @(@(120, 170, 220), @(250, 220, 190), @(255, 245, 220), @(130, 160, 170), @(80, 120, 130), @(40, 80, 90), 0.3, 0.4, @(1.4, 0.2, 3.1, 2.2)),
    @(@(140, 200, 230), @(230, 245, 220), @(255, 250, 200), @(110, 170, 120), @(60, 130, 90), @(30, 90, 70), 0.2, 0.3, @(0.1, 2.2, 1.1, 3.0)),
    @(@(60, 120, 190), @(250, 200, 140), @(255, 240, 200), @(220, 150, 90), @(190, 110, 70), @(140, 70, 50), 0.55, 0.35, @(3.3, 1.7, 0.6, 0.2)),
    @(@(60, 40, 110), @(250, 150, 170), @(255, 225, 210), @(150, 80, 140), @(100, 50, 110), @(55, 30, 75), 0.4, 0.5, @(1.9, 0.5, 2.7, 1.0)),
    @(@(8, 12, 35), @(40, 60, 110), @(235, 240, 255), @(30, 45, 80), @(20, 30, 60), @(10, 15, 35), 0.75, 0.25, @(2.6, 0.9, 0.3, 1.5))
)
for ($i = 0; $i -lt $scenes.Count; $i++) {
    $s = $scenes[$i]
    $filter = Scene $s[0] $s[1] $s[2] $s[3] $s[4] $s[5] $s[6] $s[7] $s[8] $null
    ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=c=black:s=1920x1280" -vf $filter -frames:v 1 -q:v 2 "$art\bild$i.jpg"
}
$moving = Scene $sunset[0] $sunset[1] $sunset[2] $sunset[3] $sunset[4] $sunset[5] $sunset[6] $sunset[7] $sunset[8] 0.5
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=c=black:s=960x540:r=24:d=10" -f lavfi -i "sine=frequency=220:duration=10" `
    -vf "$moving,scale=1920:1080:flags=bicubic" -c:v libx264 -pix_fmt yuv420p -preset veryfast -crf 20 -c:a aac -b:a 96k -shortest "$art\film.mp4"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=330:duration=30" -c:a libmp3lame -b:a 128k "$art\ton.mp3"

# ---------------------------------------------------------------- Beispielordner je Sprache
function Dummy([string] $path, [int] $kb) {
    $bytes = New-Object byte[] ($kb * 1024)
    (New-Object Random 7).NextBytes($bytes)
    [IO.File]::WriteAllBytes($path, $bytes)
}

function Tree([string] $base, [hashtable] $n) {
    $trip = "$base\$($n.trip)"
    New-Item -ItemType Directory -Force "$trip\$($n.raw)", "$trip\$($n.pick)", "$trip\$($n.maps)" | Out-Null
    for ($i = 0; $i -lt 6; $i++) { Copy-Item "$art\bild$i.jpg" "$trip\IMG_48$(21 + $i).JPG" }
    Copy-Item "$art\bild2.jpg" "$trip\$($n.raw)\DSC_0001.jpg"
    Copy-Item "$art\bild3.jpg" "$trip\$($n.maps)\$($n.map).jpg"
    Copy-Item "$art\film.mp4" "$trip\$($n.video).mp4"
    Copy-Item "$art\ton.mp3" "$trip\$($n.music).mp3"
    # Die Aufnahmen verteilen sich über mehrere Monate: so zeigt die Galerie ihre Monatsüberschriften.
    $taken = Get-Date '2026-11-20 16:30'
    for ($i = 0; $i -lt 6; $i++) { (Get-Item "$trip\IMG_48$(21 + $i).JPG").LastWriteTime = $taken.AddDays(-23 * $i).AddMinutes(7 * $i) }
    (Get-Item "$trip\$($n.video).mp4").LastWriteTime = $taken.AddDays(-8)
    (Get-Item "$trip\$($n.raw)\DSC_0001.jpg").LastWriteTime = $taken.AddDays(-51)
    (Get-Item "$trip\$($n.maps)\$($n.map).jpg").LastWriteTime = $taken.AddDays(-110)
    Set-Content -Path "$trip\$($n.diary).txt" -Value $n.diaryText -Encoding UTF8
    Set-Content -Path "$trip\$($n.packing).md" -Value $n.packingText -Encoding UTF8
    Dummy "$trip\$($n.costs).xlsx" 18
    Dummy "$trip\$($n.route).pdf" 240
    Compress-Archive -Path "$trip\$($n.diary).txt", "$trip\$($n.packing).md" -DestinationPath "$trip\$($n.backup).zip"

    $web = "$base\$($n.projects)\Website"
    New-Item -ItemType Directory -Force "$web\$($n.images)", "$web\$($n.drafts)" | Out-Null
    Copy-Item "$art\bild4.jpg" "$web\$($n.images)\header.jpg"
    Set-Content "$web\index.html" "<!doctype html>`n<html lang=`"$($n.code)`">`n  <head><meta charset=`"utf-8`"><title>$($n.title)</title></head>`n  <body><h1>$($n.title)</h1></body>`n</html>" -Encoding UTF8
    Set-Content "$web\style.css" ":root { --accent: #5b8def; }`nbody { font-family: system-ui, sans-serif; margin: 0; }`nh1 { color: var(--accent); }"
    Set-Content "$web\README.md" "# $($n.title)`n`n$($n.readme)" -Encoding UTF8
    Set-Content "$web\$($n.data).json" "{`n  `"name`": `"$($n.title)`",`n  `"version`": `"1.4.0`"`n}" -Encoding UTF8
    Set-Content "$web\logo.svg" "<svg xmlns=`"http://www.w3.org/2000/svg`" viewBox=`"0 0 64 64`"><circle cx=`"32`" cy=`"32`" r=`"28`" fill=`"#5b8def`"/></svg>"
    Copy-Item $exe "$web\TurboCommanderLite.exe"
    Compress-Archive -Path "$web\index.html", "$web\style.css" -DestinationPath "$web\build.zip"
}

$sets = @(
    @{
        lang = 'de'; trip = 'Urlaub 2026'; raw = 'Rohdaten'; pick = 'Auswahl'; maps = 'Karten'; map = 'Wanderkarte'
        video = 'Flug über die Küste'; music = 'Musik für den Film'; diary = 'Reisetagebuch'; packing = 'Packliste'
        costs = 'Kosten'; route = 'Route'; backup = 'Notizen gesichert'
        diaryText = "Reisetagebuch`n==============`n`nTag 1 – Ankunft am späten Nachmittag, das Licht über den Hügeln ist unglaublich.`nTag 2 – Früh raus: Morgennebel im Tal, danach Wanderung zum Aussichtspunkt.`nTag 3 – Ruhetag. Fotos sortiert und umbenannt, Drohnenflug über die Küste.`nTag 4 – Wüstenrand bei Sonnenuntergang, die Farben wechseln im Minutentakt."
        packingText = "# Packliste`n`n- Kamera, zwei Akkus, Ladegerät`n- Drohne mit Ersatzpropellern`n- Stativ`n- Wanderschuhe`n- Regenjacke"
        projects = 'Projekte'; images = 'bilder'; drafts = 'entwürfe'; data = 'daten'; code = 'de'
        title = 'Meine Website'; readme = 'Kleine Beispielseite. Mit `build.zip` wird sie hochgeladen.'
    },
    @{
        lang = 'en'; trip = 'Vacation 2026'; raw = 'Raw'; pick = 'Selection'; maps = 'Maps'; map = 'Hiking map'
        video = 'Flight along the coast'; music = 'Music for the film'; diary = 'Travel diary'; packing = 'Packing list'
        costs = 'Costs'; route = 'Route'; backup = 'Notes backup'
        diaryText = "Travel diary`n============`n`nDay 1 – Arrived late in the afternoon, the light over the hills is incredible.`nDay 2 – Up early: morning mist in the valley, then a hike to the viewpoint.`nDay 3 – Rest day. Sorted and renamed the photos, drone flight along the coast.`nDay 4 – Edge of the desert at sunset, the colors change by the minute."
        packingText = "# Packing list`n`n- Camera, two batteries, charger`n- Drone with spare propellers`n- Tripod`n- Hiking boots`n- Rain jacket"
        projects = 'Projects'; images = 'images'; drafts = 'drafts'; data = 'data'; code = 'en'
        title = 'My website'; readme = 'A small sample site. `build.zip` is what gets uploaded.'
    }
)

# ---------------------------------------------------------------- Bildschirmfotos
function Shot([string] $file, [string[]] $more) {
    Remove-Item $file -ErrorAction SilentlyContinue
    $q = [char]34
    $list = @('--shot', "$q$file$q", '--size', '1360x860') + $more
    $p = Start-Process -FilePath $exe -ArgumentList $list -PassThru
    if (-not $p.WaitForExit(60000)) { $p.Kill() }
    if (Test-Path $file) { "ok    $file" } else { "FEHLT $file" }
}

& $exe --export-icon "$root\docs\icon.png" | Out-Null
$q = [char]34
foreach ($n in $sets) {
    $base = "$demo\$($n.lang)"
    Tree $base $n
    if (Test-Path "$Drive\") { subst $Drive /d | Out-Null }
    subst $Drive $base
    try {
        $common = @('--lang', $n.lang, '--left', "$q$Drive\$($n.trip)$q", '--right', "$q$Drive\$($n.projects)\Website$q")
        $to = "$out\$($n.lang)"
        Shot "$to\main-dark.png" ($common + @('--theme', 'dark', '--demo', "${q}cursor=IMG_4821.JPG,quick$q"))
        Shot "$to\quickview-video.png" ($common + @('--theme', 'dark', '--demo', "${q}cursor=$($n.video).mp4,quick$q", '--script', "${q}sleep 2500$q"))
        Shot "$to\side-by-side-light.png" ($common + @('--theme', 'light', '--side', '--demo', "${q}cursor=$($n.diary).txt,quick$q"))
        Shot "$to\full-version.png" ($common + @('--theme', 'light', '--demo', 'full'))
        Shot "$to\thumbnails.png" ($common + @('--theme', 'dark', '--script', "${q}wait 30; key ctrl+b; sleep 3500$q"))
    } finally {
        subst $Drive /d | Out-Null
    }
}
