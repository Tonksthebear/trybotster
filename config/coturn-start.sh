#!/bin/sh
INTERNAL_IP=$(hostname -i | awk '{print $1}')
cp /etc/turnserver.conf /tmp/turnserver.conf
sed -i "s|external-ip=5.78.156.108|external-ip=5.78.156.108/$INTERNAL_IP|" /tmp/turnserver.conf
echo "static-auth-secret=$TURN_SECRET" >> /tmp/turnserver.conf
exec turnserver -c /tmp/turnserver.conf
