#!/bin/bash
# Boot the desktop: virtual screen, window manager, a VNC server on it, and the
# noVNC web client that the OpenLeash live view embeds.
export DISPLAY=:1
rm -f /tmp/.X1-lock /tmp/.X11-unix/X1
Xvfb :1 -screen 0 "${SCREEN_WIDTH:-1280}x${SCREEN_HEIGHT:-800}x24" -ac +extension RANDR -nolisten tcp &
for _ in $(seq 1 100); do xdpyinfo -display :1 >/dev/null 2>&1 && break; sleep 0.1; done
# The desktop beeps on errors, and noVNC turns every beep into a bell sample in
# the live view. Off at the source: no X bell, and x11vnc forwards none.
xset b off 2>/dev/null
dbus-launch --exit-with-session startxfce4 >/tmp/xfce.log 2>&1 &
x11vnc -display :1 -forever -shared -rfbport 5900 -passwd "${VNC_PASSWORD:-openleash}" \
  -noxdamage -nobell -quiet -localhost >/tmp/x11vnc.log 2>&1 &
exec websockify --web /usr/share/novnc 6080 127.0.0.1:5900
