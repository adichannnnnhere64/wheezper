#!/usr/bin/env bash
# Manage profiles, invites and answer cards on a deployed wheezper stack.
#
#   ./admin-profiles.sh profile <slug> <display name>      create/update a profile
#   ./admin-profiles.sh summary <slug> <file.md>           set its assistant summary
#   ./admin-profiles.sh invite  <slug> [uses] [days]       mint a pairing invite, print it once
#   ./admin-profiles.sh devices                            list paired devices + profile
#   ./admin-profiles.sh revoke <device id|name>            revoke a paired device's token
#   ./admin-profiles.sh invites                            list invites (no codes)
#   ./admin-profiles.sh cards [file.sql]                   load shared answer cards
#
# The invite code is printed once and only its sha256 is stored, so a database
# dump does not hand out working pairing codes.
set -euo pipefail
cd "$(dirname "$0")/.."
PSQL=(docker compose -f compose.deploy.yml exec -T db psql -U wheezper -d wheezper -v ON_ERROR_STOP=1)
sql() { "${PSQL[@]}" -At -c "$1"; }

case "${1:-}" in
  profile)
    [ $# -ge 3 ] || { echo "usage: $0 profile <slug> <display name>" >&2; exit 2; }
    sql "INSERT INTO profiles (id, slug, display_name) VALUES (gen_random_uuid(), '$2', \$\$$3\$\$)
         ON CONFLICT (slug) DO UPDATE SET display_name = EXCLUDED.display_name
         RETURNING id, slug, display_name;"
    ;;
  summary)
    [ $# -ge 3 ] || { echo "usage: $0 summary <slug> <file>" >&2; exit 2; }
    sql "UPDATE profiles SET summary = \$\$$(cat "$3")\$\$ WHERE slug = '$2' RETURNING slug,
         length(summary) AS chars;"
    ;;
  invite)
    [ $# -ge 2 ] || { echo "usage: $0 invite <slug> [uses] [days]" >&2; exit 2; }
    USES="${3:-1}"; DAYS="${4:-7}"
    CODE="wz-$(head -c 9 /dev/urandom | base64 | tr -dc 'a-z0-9' | head -c 12)"
    HASH=$(printf '%s' "$CODE" | sha256sum | cut -d' ' -f1)
    sql "INSERT INTO invites (id, code_hash, profile_id, label, max_uses, expires_at)
         SELECT gen_random_uuid(), '$HASH', id, '$2 invite', $USES, now() + interval '$DAYS days'
         FROM profiles WHERE slug = '$2' RETURNING id;" >/dev/null
    echo "invite for profile '$2' (uses=$USES, expires in ${DAYS}d):"
    echo "  $CODE"
    echo "Enter that in the phone's Settings → pairing code."
    ;;
  devices)
    "${PSQL[@]}" -c "SELECT left(d.id::text, 8) AS id, d.name, p.slug, d.created_at::date, d.expires_at::date
                     FROM devices d LEFT JOIN profiles p ON p.id = d.profile_id
                     ORDER BY d.created_at DESC LIMIT 20;"
    ;;
  revoke)
    [ $# -ge 2 ] || { echo "usage: $0 revoke <device id|name>" >&2; exit 2; }
    TARGET=${2//\'/\'\'}
    OUT=$("${PSQL[@]}" -At -c "WITH gone AS (
                                 DELETE FROM devices
                                 WHERE id::text = '$TARGET' OR id::text LIKE '$TARGET%' OR name = '$TARGET'
                                 RETURNING id || ' ' || name)
                               SELECT * FROM gone;")
    if [ -z "$OUT" ]; then echo "no device matched '$2'" >&2; exit 1; fi
    echo "revoked: $OUT"
    echo "The token answers Unauthenticated on its next request and has to be re-paired."
    ;;
  invites)
    "${PSQL[@]}" -c "SELECT p.slug, i.label, i.uses, i.max_uses, i.expires_at::date AS expires
                     FROM invites i JOIN profiles p ON p.id = i.profile_id
                     ORDER BY i.created_at DESC LIMIT 20;"
    ;;
  cards)
    FILE="${2:-scripts/seed-profiles-cards.sql}"
    "${PSQL[@]}" -f "/dev/stdin" < "$FILE"
    sql "SELECT topic, count(*) FROM answer_cards GROUP BY topic ORDER BY topic;"
    ;;
  *)
    sed -n '2,15p' "$0"; exit 2
    ;;
esac
