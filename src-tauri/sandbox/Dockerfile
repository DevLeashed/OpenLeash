# The desktop an OpenLeash sandbox runs by default: Ubuntu with XFCE, a virtual
# screen and a VNC server the live view embeds. Built once by OpenLeash before
# the first sandbox starts, and shared by every sandbox; each sandbox is a
# container of its own made from it. A custom image can replace it (Settings).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8
RUN apt-get update && apt-get install -y --no-install-recommends \
      software-properties-common gpg-agent ca-certificates \
    && add-apt-repository -y ppa:mozillateam/ppa \
    && printf 'Package: *\nPin: release o=LP-PPA-mozillateam\nPin-Priority: 1001\n' > /etc/apt/preferences.d/mozilla-firefox \
    && apt-get update && apt-get install -y --no-install-recommends \
      xvfb x11vnc x11-utils x11-xserver-utils xdotool imagemagick xclip \
      xfce4 xfce4-terminal thunar mousepad dbus-x11 novnc websockify \
      firefox fonts-liberation fonts-noto-color-emoji \
      sudo curl wget git python3 python3-pip nano procps \
    && apt-get purge -y xfce4-screensaver light-locker 2>/dev/null; \
    rm -rf /var/lib/apt/lists/*

# 24.04 ships an `ubuntu` user at uid 1000; the sandbox user takes its place.
RUN (userdel -r ubuntu 2>/dev/null || true) \
    && useradd -m -s /bin/bash -u 1000 agent \
    && echo 'agent ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/agent \
    && mkdir -p /home/agent/shared && chown agent:agent /home/agent/shared
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

USER agent
WORKDIR /home/agent
ENV DISPLAY=:1 HOME=/home/agent
EXPOSE 6080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
