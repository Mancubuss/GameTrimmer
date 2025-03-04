#include "steamlibrary.h"
#include <QSettings>
#include <QFile>
#include <QDir>
#include <QDebug>
#include <QTextStream>
#include <QRegularExpression>
#include <QFileInfo>

QString SteamLibrary::findSteamPath()
{
    QStringList registryPaths = {
        // 64-bit
        "HKEY_LOCAL_MACHINE\\SOFTWARE\\Wow6432Node\\Valve\\Steam",
        // 32-bit
        "HKEY_LOCAL_MACHINE\\SOFTWARE\\Valve\\Steam"
    };

    // Шукаємо в реєстрі
    for (const QString &regPath : registryPaths) {
        QSettings settings(regPath, QSettings::NativeFormat);
        QString steamPath = settings.value("InstallPath").toString();
        if (!steamPath.isEmpty()) {
            QDir dir(steamPath);
            if (dir.exists()) {
                return dir.absolutePath();
            }
        }
    }

    // Перевіряємо стандартні шляхи
    QStringList defaultPaths = {
        "C:/Program Files (x86)/Steam",
        "C:/Program Files/Steam"
    };

    for (const QString &path : defaultPaths) {
        QDir dir(path);
        if (dir.exists()) {
            return dir.absolutePath();
        }
    }

    return QString();
}

QStringList SteamLibrary::findLibraryFolders(const QString &steamPath)
{
    QStringList libraries;
    QDir steamDir(steamPath);
    
    // Додаємо основну теку Steam
    QString mainLibrary = steamDir.absoluteFilePath("steamapps");
    if (QDir(mainLibrary).exists()) {
        libraries << QDir::cleanPath(mainLibrary);
    }
    
    // Шукаємо додаткові бібліотеки в libraryfolders.vdf
    QString vdfPath = steamDir.absoluteFilePath("steamapps/libraryfolders.vdf");
    QFile file(vdfPath);
    
    if (!file.open(QIODevice::ReadOnly | QIODevice::Text)) {
        qDebug() << "Не вдалося відкрити файл:" << vdfPath;
        return libraries;
    }
    
    QTextStream in(&file);
    QString content = in.readAll();
    file.close();
    
    // Парсимо VDF формат
    QRegularExpression pathRegex("\"path\"\\s+\"([^\"]+)\"");
    QRegularExpressionMatchIterator i = pathRegex.globalMatch(content);
    
    while (i.hasNext()) {
        QRegularExpressionMatch match = i.next();
        QString path = match.captured(1);
        // Замінюємо подвійні зворотні слеші на прямі
        path = QDir::fromNativeSeparators(path);
        QString libraryPath = QDir::cleanPath(path + "/steamapps");
        if (QDir(libraryPath).exists() && !libraries.contains(libraryPath)) {
            libraries << libraryPath;
        }
    }
    
    return libraries;
}

QMap<QString, QString> SteamLibrary::parseVdfFile(const QString &path)
{
    QMap<QString, QString> result;
    QFile file(path);
    
    if (!file.open(QIODevice::ReadOnly | QIODevice::Text)) {
        qDebug() << "Не вдалося відкрити файл:" << path;
        return result;
    }
    
    QTextStream in(&file);
    QString content = in.readAll();
    file.close();
    
    // Парсимо прості пари ключ-значення
    QRegularExpression keyValueRegex("\"([^\"]+)\"\\s+\"([^\"]+)\"");
    QRegularExpressionMatchIterator i = keyValueRegex.globalMatch(content);
    
    while (i.hasNext()) {
        QRegularExpressionMatch match = i.next();
        result[match.captured(1)] = match.captured(2);
    }
    
    return result;
} 