#!/bin/sh

chown user:user /data
exec su -s /bin/sh user -c "$@"
