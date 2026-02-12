#!/bin/sh
cp /etc/turnserver.conf /tmp/turnserver.conf
echo "static-auth-secret=$TURN_SECRET" >> /tmp/turnserver.conf
exec turnserver -c /tmp/turnserver.conf
