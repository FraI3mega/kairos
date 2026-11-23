FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      xvfb xauth openbox xdotool xterm x11-utils \
      sway foot fonts-dejavu-core \
      dbus gjs procps \
 && rm -rf /var/lib/apt/lists/*
