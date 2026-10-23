#!/bin/bash
# Default-deny egress. The allowed hosts are resolved once and pinned in /etc/hosts, and DNS is
# then blocked too, so nothing can tunnel out through the resolver. Runs as root once at start.
set -euo pipefail

ready=/run/reeve-ready
[ "${REEVE_SANDBOX_NET:-restricted}" = open ] && { touch "$ready"; exit 0; }

hosts="api.anthropic.com ${REEVE_SANDBOX_ALLOW:-}"

ipset create allowed hash:ip
for h in $hosts; do
  ips=$(dig +short +time=2 +tries=2 A "$h" | grep -E '^[0-9.]+$' || true)
  [ -n "$ips" ] || { echo "cannot resolve $h" >&2; exit 1; }
  for ip in $ips; do
    ipset add -exist allowed "$ip"
    echo "$ip $h" >> /etc/hosts
  done
done

ip6tables -A OUTPUT -o lo -j ACCEPT
ip6tables -P OUTPUT DROP
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m set --match-set allowed dst -j ACCEPT
iptables -A OUTPUT -j REJECT --reject-with icmp-admin-prohibited
iptables -P OUTPUT DROP

dig +short +time=1 +tries=1 example.com >/dev/null 2>&1 && { echo "firewall check failed: DNS reachable" >&2; exit 1; }
curl -s --connect-timeout 2 -o /dev/null https://1.1.1.1 && { echo "firewall check failed: 1.1.1.1 reachable" >&2; exit 1; }
curl -s --max-time 10 -o /dev/null https://api.anthropic.com || { echo "firewall check failed: api.anthropic.com unreachable" >&2; exit 1; }
touch "$ready"
