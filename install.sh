#!/bin/bash

echo "Copying Startup and Shudown scripts to /usr/bin/ and making executable"
sudo cp ./bin/LG_Buddy_Startup /usr/bin/
sudo cp ./bin/LG_Buddy_Shutdown /usr/bin/
sudo chmod +x /usr/bin/LG_Buddy_Startup
sudo chmod +x /usr/bin/LG_Buddy_Shutdown
echo "Done."
echo "Copying systemd services to /etc/systemd/system/"
sudo cp ./systemd/LG_Buddy.service /etc/systemd/system/
echo "Done."
echo "Enabling service..."
sudo systemctl daemon-reload
sudo systemctl enable LG_Buddy.service
sudo systemctl is-enabled LG_Buddy.service
echo "Done."
