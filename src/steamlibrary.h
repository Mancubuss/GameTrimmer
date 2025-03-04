#pragma once

#include <QString>
#include <QStringList>
#include <QMap>

class SteamLibrary {
public:
    static QString findSteamPath();
    static QStringList findLibraryFolders(const QString &steamPath);
    
private:
    static QString findWindowsSteamPath();
    static QMap<QString, QString> parseVdfFile(const QString &path);
}; 