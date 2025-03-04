#pragma once

#include <QString>
#include <QSqlDatabase>

class Database {
public:
    Database();
    ~Database();
    
    bool open();
    void close();
    
    bool addLibrary(const QString &path);
    bool removeLibrary(const QString &path);
    QStringList getLibraries();
    
    bool addFile(const QString &path, qint64 size, const QString &type);
    bool removeFile(const QString &path);
    void clearFiles();
    
private:
    QSqlDatabase db;
    
    bool createTables();
}; 