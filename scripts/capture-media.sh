#!/bin/bash
# Captures the README screenshots and GIFs into media/: run `make media`, which builds the
# harness first.
#
# The real panel runs against a throwaway home, /Users/Shared/Demo, containing invented documents,
# clipboard entries and snippets, with only Apple's built-in apps. A backdrop window covers the
# screen.
#
# The harness also runs in a sandbox that denies Spotlight. Recent files and plain-word file search
# query Spotlight across the whole disk, not $HOME, and an unsandboxed first run showed real file
# names. The harness additionally checks every row it captures and fails on any path outside the
# demo home.
#
# /Users/Shared rather than /tmp, because result rows print full folder paths and this one reads
# like a real home. The installed app does not index /Users/Shared.
#
# Needs:
# - Screen Recording permission for the terminal.
# - ffmpeg for the GIFs.
# - Ollama with a local model for the AI answer. Without it, that scene is skipped.
#
# Keep hands off the mouse and keyboard while it runs, about three minutes: clicking elsewhere
# closes the panel.
set -euo pipefail

harness=${1:?usage: scripts/capture-media.sh path/to/media-capture}
cd "$(git rev-parse --show-toplevel)"
out="$PWD/${MEDIA_OUT:-media}"
work="$PWD/build/media/work"
model=${MEDIA_MODEL:-qwen3.5:4b-mlx}

command -v ffmpeg >/dev/null || { echo "error: ffmpeg is required (brew install ffmpeg)" >&2; exit 1; }
home=/Users/Shared/Demo
if [[ -e $home ]]; then
    echo "error: $home already exists; move it aside first (this script creates and deletes it)" >&2
    exit 1
fi
mkdir "$home"
trap 'rm -rf "$home"' EXIT
rm -rf "$work"
mkdir -p "$work" "$out"

ai=0
if curl -fsS --max-time 2 http://127.0.0.1:11434/api/tags 2>/dev/null | grep -q "\"$model\""; then
    ai=1
else
    echo "Ollama or $model is not available: skipping the AI answer scene."
fi

mkdir -p "$home/.config/blindspot"
cat >"$home/.config/blindspot/config.toml" <<EOF
max_results = 8
app_paths = ["/System/Applications", "/System/Applications/Utilities"]
launch_at_login = false

[agent]
enabled = true
model = "$model"
question_model = "$model"
roots = ["~/Documents", "~/Desktop"]

[content]
enabled = true
semantic = true
documents = true
on_battery = true
roots = ["~/Documents", "~/Desktop"]

[clips]
enabled = true
EOF

# Invented documents. Northwind Traders is a fictional company; every name, number and address
# below is made up for the screenshots.
docs="$home/Documents"
mkdir -p "$docs/Clients/Northwind" "$docs/Notes" "$docs/Finance" "$home/Desktop"
text() { mkdir -p "$(dirname "$1")"; cat >"$1"; }

text "$work/proposal.txt" <<'EOF'
Northwind Traders: Renewal Proposal 2027
Prepared by Alex Martin, September 2026

Summary
Northwind's annual subscription renews on October 31, 2026. We propose a two-year renewal that includes the analytics add-on at no extra cost for the first six months.

What changes
Pricing moves from per-seat to per-site licensing across their 12 sites. Support response time improves from 8 to 4 business hours. The on-premise connector is replaced by the cloud gateway.

Next steps
Legal review by October 10, signature by October 24.
EOF
textutil -convert docx -output "$docs/Clients/Northwind/Northwind renewal proposal.docx" "$work/proposal.txt"

text "$work/review.txt" <<'EOF'
Northwind Traders: Q3 2026 Account Review

Usage grew 18% quarter over quarter across 12 sites. Two support escalations, both resolved within the service agreement.

Risks
The renewal deadline of October 31 overlaps with Northwind's ERP migration. Schedule the legal review early and confirm the signer before October 17.
EOF
cupsfilter -i text/plain -m application/pdf "$work/review.txt" >"$docs/Clients/Northwind/Q3 account review.pdf" 2>/dev/null

text "$work/invoice.txt" <<'EOF'
INVOICE INV-2041

Bill to: Northwind Traders, Accounts Payable
Service period: October 2025 to September 2026
Platform subscription, 12 sites     $42,000.00
Premium support                      $6,600.00
Total due                           $48,600.00 CAD
Payment due October 15, 2026
EOF
cupsfilter -i text/plain -m application/pdf "$work/invoice.txt" >"$docs/Finance/Invoice INV-2041 Northwind.pdf" 2>/dev/null

text "$docs/Notes/2026-09-02 standup.md" <<'EOF'
# Standup, September 2

- Northwind: still waiting on their legal contact for the renewal redlines
- Capstone: signaling server merged; TURN fallback next
- Offsite: book the room for the Montréal team day
EOF

text "$docs/Notes/Capstone signaling decision.md" <<'EOF'
# Capstone: WebRTC signaling decision

We chose a small WebSocket signaling server over MQTT. It is one less broker to run, the browser
support is native, and reconnects are simpler. Media still flows peer to peer, with a TURN relay
as the fallback when a network blocks direct connections.
EOF

text "$home/Desktop/Montreal offsite checklist.txt" <<'EOF'
Montréal offsite checklist
- Confirm the room at 1250 Rue University
- Print the Northwind renewal summary for the account planning session
- Order lunch for 14
EOF
rm -f "$work"/*.txt

# Two phases. Indexing runs unsandboxed because the extraction helper's own no-network sandbox
# cannot nest inside another sandbox: nested, every PDF and Word file comes back unreadable. That
# phase shows nothing and never queries Spotlight. The capture phase then runs sandboxed and
# reuses the index, since unchanged files are not extracted again.
echo "Indexing the demo documents…"
HOME="$home" MEDIA_PHASE=index "$harness"

echo "Capturing into ${out#"$PWD/"}; keep hands off the mouse and keyboard…"
no_spotlight='(version 1)(allow default)(deny mach-lookup (global-name-regex #"^com\.apple\.metadata\."))'
# A failed scene should not throw away the recordings that succeeded, so convert them first.
status=0
HOME="$home" MEDIA_OUT="$out" MEDIA_WORK="$work" MEDIA_AI="$ai" \
    /usr/bin/sandbox-exec -p "$no_spotlight" "$harness" || status=$?

for recording in "$work"/*.mov; do
    [[ -e $recording ]] || continue
    name=$(basename "$recording" .mov)
    # Skip the recorder's lead-in, then a shared palette per GIF keeps text crisp and files small.
    ffmpeg -loglevel error -y -ss 1.2 -i "$recording" \
        -vf "fps=12,scale=900:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=160:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
        -loop 0 "$out/$name.gif"
done
rm -rf "$work"

echo
echo "Media in ${out#"$PWD/"}:"
ls -lh "$out" | awk 'NR > 1 { print "  " $5 "\t" $9 }'
exit "$status"
