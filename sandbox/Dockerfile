FROM node:24.22.1-trixie-slim@sha256:43b9ba1142b8dcf33fa8f922ca1be85714adc444e0586c7cf7726daaeabc1bea

RUN apt-get update && apt-get install -y --no-install-recommends \
    git ca-certificates curl dnsutils iptables ipset iproute2 ripgrep jq less procps \
  && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION
RUN test -n "$CLAUDE_CODE_VERSION" && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" && npm cache clean --force

COPY init-firewall.sh /usr/local/bin/init-firewall
RUN chmod 755 /usr/local/bin/init-firewall

ENV DISABLE_AUTOUPDATER=1 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1

USER node
RUN git config --global user.name sandbox && git config --global user.email sandbox@localhost \
  && git config --global safe.directory '*'
WORKDIR /home/node
CMD ["sleep", "infinity"]
