#!/bin/sh
# Stand up (or upgrade) a GlobalObject control plane with Docker Compose.
#
#   ./deploy/install-server.sh --domain cp.example.org
#   curl -fsSL https://raw.githubusercontent.com/<owner>/<repo>/main/deploy/install-server.sh | sh -s -- --domain cp.example.org
#
# Options:
#   --domain NAME   public DNS name; Caddy gets an HTTPS certificate for it (ports 80, 443)
#   --no-tls        no Caddy: serve plain HTTP on port 7400 (LAN or testing only)
#   --monitoring    also run Prometheus with alert rules (127.0.0.1:9090)
#   --dir DIR       where to keep the Compose files when not run from a checkout
#
# Re-running upgrades: it pulls or rebuilds images and restarts, keeping .env and data.
set -eu

say() { printf '%s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

domain=""
tls=1
monitoring=0
dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --domain) [ $# -ge 2 ] || die "--domain needs a value"; domain="$2"; shift ;;
    --no-tls) tls=0 ;;
    --monitoring) monitoring=1 ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift ;;
    -h | --help) sed -n '2,15p' "$0" 2>/dev/null || true; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

command -v docker >/dev/null 2>&1 || die "Docker is needed: https://docs.docker.com/engine/install/"
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is needed (the 'docker compose' command)"
docker info >/dev/null 2>&1 || die "can't talk to Docker: is it running, and are you in the docker group?"

# From a checkout, use its files (and build the image from source). Otherwise fetch them.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)"
if [ -n "$here" ] && [ -f "$here/compose/docker-compose.yml" ] && [ -z "$dir" ]; then
  dir="$here/compose"
else
  repo="${GLOBALOBJECT_REPO:-}"
  [ -n "$repo" ] || die "not in a checkout: set GLOBALOBJECT_REPO=owner/name, or clone the repository and run deploy/install-server.sh"
  ref="${GLOBALOBJECT_REF:-main}"
  dir="${dir:-$HOME/globalobject-server}"
  mkdir -p "$dir"
  for f in docker-compose.yml Caddyfile prometheus.yml alerts.yml; do
    curl -fsSL "https://raw.githubusercontent.com/$repo/$ref/deploy/compose/$f" -o "$dir/$f" || die "couldn't fetch $f"
  done
  [ -n "${GLOBALOBJECT_IMAGE:-}" ] || GLOBALOBJECT_IMAGE="ghcr.io/$(printf '%s' "$repo" | tr '[:upper:]' '[:lower:]')-control-plane:latest"
fi
cd "$dir"
env_file="$dir/.env"

# Keep an existing .env: it holds the operator token, and changing it locks out the admin CLI.
if [ -f "$env_file" ]; then
  say "Using existing $env_file"
  # shellcheck disable=SC1090
  . "$env_file"
  [ -z "$domain" ] || [ "$domain" = "${GLOBALOBJECT_DOMAIN:-}" ] || say "note: --domain differs from GLOBALOBJECT_DOMAIN in .env; edit .env to change it"
else
  if [ "$tls" = 1 ] && [ -z "$domain" ]; then
    if [ -r /dev/tty ]; then
      printf 'Public domain name for this server (DNS must point here), or leave empty for plain HTTP: ' >/dev/tty
      read -r domain </dev/tty || true
    fi
    [ -n "$domain" ] || tls=0
  fi
  token="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  profiles=""
  if [ "$tls" = 1 ]; then
    profiles="tls"
    public_url="https://$domain"
    bind=127.0.0.1
  else
    host="$(hostname -I 2>/dev/null | cut -d' ' -f1)"
    public_url="http://${host:-127.0.0.1}:7400"
    bind=0.0.0.0
  fi
  [ "$monitoring" = 0 ] || profiles="${profiles:+$profiles,}monitoring"
  umask 077
  cat >"$env_file" <<EOF
# GlobalObject control plane settings. Keep this file private: the token controls the server.
GLOBALOBJECT_OPERATOR_TOKEN=$token
GLOBALOBJECT_DOMAIN=${domain:-unused}
GLOBALOBJECT_PUBLIC_URL=$public_url
CONTROL_PLANE_BIND=$bind
COMPOSE_PROFILES=$profiles
${GLOBALOBJECT_IMAGE:+GLOBALOBJECT_IMAGE=$GLOBALOBJECT_IMAGE}
EOF
  umask 022
  say "Wrote $env_file (it holds the operator token)"
  # shellcheck disable=SC1090
  . "$env_file"
fi

say "Starting MongoDB, the control plane${COMPOSE_PROFILES:+ and $COMPOSE_PROFILES}..."
if [ -f ../../Cargo.toml ] && [ -z "${GLOBALOBJECT_IMAGE:-}" ]; then
  docker compose build control-plane
else
  docker compose pull --quiet control-plane || die "couldn't pull ${GLOBALOBJECT_IMAGE:-the image}"
fi
docker compose up -d --remove-orphans

say "Waiting for the control plane to be ready..."
i=0
until curl -fsS http://127.0.0.1:7400/readyz >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -lt 90 ] || die "not ready after 3 minutes: see 'docker compose logs control-plane mongo' in $dir"
  sleep 2
done

admin() { docker compose exec -T control-plane globalobject-control-plane admin "$@"; }
if [ "$(admin pools | wc -l)" -le 1 ]; then
  say ""
  say "Creating the first pool..."
  admin create-pool "Main"
fi

say ""
say "The control plane is running at $GLOBALOBJECT_PUBLIC_URL"
say "  Operator dashboard:  $GLOBALOBJECT_PUBLIC_URL/admin   (token: GLOBALOBJECT_OPERATOR_TOKEN in $env_file)"
say "  Invite someone:      cd $dir && docker compose exec control-plane globalobject-control-plane admin invite --pool 1"
say "  Health:              cd $dir && docker compose exec control-plane globalobject-control-plane admin status"
say "  Database backups:    nightly, in the 'mongobackup' volume (copy them off this host too)"
[ "${COMPOSE_PROFILES#*monitoring}" = "${COMPOSE_PROFILES}" ] || say "  Prometheus:          http://127.0.0.1:9090 (alerts in alerts.yml)"
