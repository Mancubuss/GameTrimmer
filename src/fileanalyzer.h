#pragma once

#include <QString>
#include <QStringList>
#include <QRegularExpression>

class FileAnalyzer {
public:
    FileAnalyzer();
    
    bool isRedistributable(const QString &path) const;
    bool isLanguageFile(const QString &path) const;
    
private:
    QList<QRegularExpression> redistFolderPatterns;
    QList<QRegularExpression> redistFilePatterns;
    QList<QRegularExpression> extraFolderPatterns;
    QList<QRegularExpression> extraFilePatterns;
    
    void initializePatterns();
}; 