# Пакет из готового дерева установки (scripts/package-in-container.sh): бинарь собран один раз
# для AppImage, deb и rpm.
Name:           melogold
Version:        %{melogold_version}
Release:        1%{?dist}
Summary:        Music from YouTube Music and YouTube
License:        GPL-3.0-or-later
URL:            https://github.com/melogold-app/melogoldLinux
Requires:       gtk4 >= 4.20
Requires:       libadwaita >= 1.8
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
# Декодер AAC: fdk-aac из bad-free обычно уже стоит; libav тянет ffmpeg.
Requires:       (gstreamer1-plugins-bad-free or gstreamer1-plugin-libav)
Recommends:     xdg-desktop-portal

%global debug_package %{nil}
%global __strip /bin/true

%description
Melogold plays YouTube Music and YouTube with one library on all your devices:
favorites, playlists, history and lyrics sync through the Melogold server.

%install
cp -a %{melogold_stage}/. %{buildroot}/

%files
%license /usr/share/licenses/melogold/LICENSE
/usr/bin/melogold
/usr/share/applications/app.melogold.Melogold.desktop
/usr/share/metainfo/app.melogold.Melogold.metainfo.xml
/usr/share/icons/hicolor/*/apps/app.melogold.Melogold.png
