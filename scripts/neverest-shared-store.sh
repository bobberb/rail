#!/usr/bin/env bash
# End-to-end check of Rail's store layout (docs/design/pimdir-port.md, decision 4): two
# neverest accounts syncing two real IMAP mailboxes into ONE pimdir store, then Rail's
# rail-pimdir reading it back (crates/rail-pimdir/tests/neverest.rs).
#
# Needs podman or docker, curl, jq, and a neverest built at the io-pimdir rev Rail pins:
#
#   NEVEREST=/path/to/neverest scripts/neverest-shared-store.sh
#
# It starts a throwaway Stalwart v0.16 server (as neverest's own tests/stalwart.sh does, on
# unprivileged ports), creates alice@rail.test and bob@rail.test, appends mail to each inbox,
# syncs both accounts into one store.root, and runs the Rust check against that store. The
# container and the scratch directory are removed on exit.

set -euo pipefail

NEVEREST=${NEVEREST:-neverest}
ENGINE=${ENGINE:-$(command -v podman || command -v docker || true)}
[ -n "$ENGINE" ] || { echo "needs podman or docker (or ENGINE=/path/to/one)" >&2; exit 1; }
# Rootless podman without a systemd user session (CI, sandboxes) cannot ask systemd for a
# cgroup; cgroupfs works everywhere. Docker takes no such flag. A function, not an array:
# an empty array under `set -u` aborts on bash 3.2, macOS's.
engine() {
    case "$(basename "$ENGINE")" in
        podman) "$ENGINE" --cgroup-manager=cgroupfs "$@" ;;
        *) "$ENGINE" "$@" ;;
    esac
}
# SELinux hosts need the bind mount relabelled for the container. Done for podman, the engine
# SELinux distributions ship; docker gets the plain mount, as neverest's own script does.
MOUNT_OPTS=ro
[ "$(basename "$ENGINE")" = podman ] && MOUNT_OPTS=ro,Z
NAME=rail-shared-store-$$
ADMIN_PASS=test
PASS='R@il-shared-store-2026'
ADMIN_PORT=${ADMIN_PORT:-18080}
IMAP_PORT=${IMAP_PORT:-11143}
IMAGE=docker.io/stalwartlabs/stalwart:v0.16-alpine
WORK=$(mktemp -d)
STORE="$WORK/store"

cleanup() {
    engine rm -f "$NAME" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

"$NEVEREST" --version >/dev/null || { echo "no neverest at $NEVEREST" >&2; exit 1; }

jmap() { curl -fsS -u "admin:$ADMIN_PASS" -H 'Content-Type: application/json' -d "$1" "http://127.0.0.1:$ADMIN_PORT/jmap/"; }
wait_for() { for _ in $(seq 1 60); do eval "$1" >/dev/null 2>&1 && return 0; sleep 1; done; echo "timed out: $1" >&2; return 1; }

# --- server -------------------------------------------------------------------------------
printf '{"@type":"RocksDb","path":"/var/lib/stalwart/data"}\n' > "$WORK/config.json"
chmod 644 "$WORK/config.json"
engine run -d --name "$NAME" \
    -e "STALWART_RECOVERY_ADMIN=admin:$ADMIN_PASS" \
    -v "$WORK/config.json:/etc/stalwart/config.json:$MOUNT_OPTS" \
    -p "127.0.0.1:$ADMIN_PORT:8080" -p "127.0.0.1:$IMAP_PORT:993" "$IMAGE" >/dev/null
wait_for "curl -fsS -u admin:$ADMIN_PASS http://127.0.0.1:$ADMIN_PORT/.well-known/jmap"

acc=$(curl -fsS -u "admin:$ADMIN_PASS" "http://127.0.0.1:$ADMIN_PORT/.well-known/jmap" | jq -r '.accounts | keys[0]')
imaps=$(jmap "{\"using\":[\"urn:ietf:params:jmap:core\",\"urn:stalwart:jmap\"],
  \"methodCalls\":[[\"x:NetworkListener/query\",{\"accountId\":\"$acc\",\"filter\":{\"name\":\"imaps\"}},\"0\"]]}" |
  jq -r '.methodResponses[0][1].ids[0]')
jmap "{\"using\":[\"urn:ietf:params:jmap:core\",\"urn:stalwart:jmap\"],\"methodCalls\":[
  [\"x:Domain/set\",{\"accountId\":\"$acc\",\"create\":{\"d\":{\"name\":\"rail.test\"}}},\"0\"],
  [\"x:Account/set\",{\"accountId\":\"$acc\",\"create\":{
    \"a\":{\"@type\":\"User\",\"name\":\"alice\",\"domainId\":\"#d\",\"credentials\":{\"0\":{\"@type\":\"Password\",\"secret\":\"$PASS\"}}},
    \"b\":{\"@type\":\"User\",\"name\":\"bob\",\"domainId\":\"#d\",\"credentials\":{\"0\":{\"@type\":\"Password\",\"secret\":\"$PASS\"}}}}},\"1\"],
  [\"x:NetworkListener/set\",{\"accountId\":\"$acc\",\"update\":{\"$imaps\":{\"useTls\":false,\"tlsImplicit\":false}}},\"2\"],
  [\"x:Imap/set\",{\"accountId\":\"$acc\",\"update\":{\"singleton\":{\"allowPlainTextAuth\":true}}},\"3\"]]}" |
  jq -e '[.methodResponses[] | .[1] | (.created // .updated // {}) | length > 0] | all' >/dev/null
engine restart "$NAME" >/dev/null
wait_for "bash -c 'echo > /dev/tcp/127.0.0.1/$IMAP_PORT'"
# Logins work once the restarted server has read its settings. A listing is safe to retry,
# where an APPEND retried after a server-side success would file the message twice.
for user in alice bob; do
    wait_for "curl -fsS --url imap://127.0.0.1:$IMAP_PORT/ -u '$user@rail.test:$PASS'"
done

# --- mail ---------------------------------------------------------------------------------
append() { # user id parent from subject date
    local parent=""
    [ -n "$3" ] && parent="In-Reply-To: <$3>"$'\r\n'"References: <$3>"$'\r\n'
    printf 'Message-ID: <%s>\r\n%sFrom: %s\r\nTo: %s@rail.test\r\nSubject: %s\r\nDate: %s\r\n\r\nHello.\r\n' \
        "$2" "$parent" "$4" "$1" "$5" "$6" > "$WORK/msg.eml"
    curl -fsS --url "imap://127.0.0.1:$IMAP_PORT/INBOX" -u "$1@rail.test:$PASS" -T "$WORK/msg.eml"
}
append alice plans@rail.test ""                "Bob <bob@rail.test>"     "Plans"     "Mon, 03 Aug 2026 09:00:00 +0000"
append alice reply@rail.test plans@rail.test "Carol <carol@rail.test>" "Re: Plans" "Mon, 03 Aug 2026 10:00:00 +0000"
append bob   lunch@rail.test ""                "Alice <alice@rail.test>" "Lunch?"    "Mon, 03 Aug 2026 11:00:00 +0000"

# --- sync: two accounts, one store.root, a source named for each account ------------------
source_block() { # account
    cat <<EOF
[accounts.$1]
store.root = "$STORE"
sources.$1-imap.imap.server = "imap://127.0.0.1:$IMAP_PORT"
sources.$1-imap.imap.starttls = false
sources.$1-imap.imap.sasl.plain.username = "$1@rail.test"
sources.$1-imap.imap.sasl.plain.password.raw = "$PASS"
sources.$1-imap.imap.collection.filter.include = ["INBOX"]
# Declared so the source's capabilities include sending, as Rail's fixtures model it. Nothing
# is submitted, so the server is never contacted.
sources.$1-imap.smtp.server = "smtp://127.0.0.1:2525"

EOF
}
{ source_block alice; source_block bob; } > "$WORK/neverest.toml"

for account in alice bob; do
    "$NEVEREST" -c "$WORK/neverest.toml" init -a "$account"
    "$NEVEREST" -c "$WORK/neverest.toml" sync -a "$account"
done

# --- read it back the way Rail does -------------------------------------------------------
cd "$(dirname "$0")/.."
RAIL_NEVEREST_STORE="$STORE" cargo test -q -p rail-pimdir --test neverest -- --ignored --nocapture
echo "shared store: OK"
