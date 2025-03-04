#pragma once

#include <QMainWindow>
#include <QStandardItemModel>

class QFileInfo;

QT_BEGIN_NAMESPACE
namespace Ui { class MainWindow; }
QT_END_NAMESPACE

class MainWindow : public QMainWindow
{
    Q_OBJECT

public:
    MainWindow(QWidget *parent = nullptr);
    ~MainWindow();

private slots:
    void on_locateSteamButton_clicked();
    void on_autoLocateButton_clicked();
    void on_searchButton_clicked();
    void on_removeSelectedButton_clicked();

private:
    Ui::MainWindow *ui;
    QStandardItemModel *filesModel;
    
    void setupModels();
    void scanDirectory(const QString &path);
    qint64 getDirSize(const QString &path);
    QString getFileType(const QFileInfo &fileInfo);
}; 