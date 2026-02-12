#!/bin/sh
exec turnserver -c /etc/turnserver.conf --static-auth-secret="$TURN_SECRET"
