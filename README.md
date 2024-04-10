# LG Buddy
Inspired by LG Companion for Windows (https://github.com/JPersson77/LGTVCompanion), LG Buddy is a set of scripts and service using https://github.com/chros73/bscpylgtv to turn LG WebOS TV's on and off automatically on startup and shutdown in Linux.

# PREREQUISITES #

Before installation, you will need Python and PIP packages installed. Get these using your distro's package manager. Then we install bscpylgtv using pip.

For Arch based distros for example, you can run these commands to install:

1. INSTALL PYTHON AND PIP:
```
  sudo pacman -S python python-pip
```
2. CREATE A VIRUTAL ENVIRONMENT FOR LG_BUDDY:
```
  sudo python -m venv /usr/bin/LG_Buddy_PIP
```
3. INSTALL BSCPYLGTV INTO OUR VIRTUAL ENVIRONMENT:
```
  sudo /usr/bin/LG_Buddy_PIP/bin/pip install bscpylgtv
```
# INSTALLATION #

1. Download the latest release of LG Buddy.

2. In the 'bin' folder, Open both files 'LG_Buddy_Startup' and 'LG_Buddy_Shutdown' in your text editor and set the variables at the top of the files for your TV. Particularly the IP address of your TV and the Input you use for your PC.

3. Set install.sh as executable
```
    sudo chmod -x ./install.sh
```
4. Run install.sh
```
    sh ./install.sh
```
5. Enter root password. Install should now be complete.

Restart your computer


Author:
Rob Grieves aka https://github.com/Faceless3882 aka r/TheFacIessMan

Credit and Thanks:
https://github.com/chros73 for the bscpylgtv software that makes this possible.

https://github.com/JPersson77 for the inspiration and pointing me in the right direction.
