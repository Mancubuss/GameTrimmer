#include "mainwindow.h"
#include "./ui_mainwindow.h"
#include "steamlibrary.h"

#include <QFileDialog>
#include <QMessageBox>
#include <QStandardItemModel>
#include <QDirIterator>
#include <QFileInfo>
#include <QIcon>

MainWindow::MainWindow(QWidget *parent)
    : QMainWindow(parent)
    , ui(new Ui::MainWindow)
    , filesModel(new QStandardItemModel(this))
{
    ui->setupUi(this);
    setupModels();
    
    // Встановлюємо іконку вікна
    setWindowIcon(QIcon(":/gametrimmer.ico"));
}

MainWindow::~MainWindow()
{
    delete ui;
}

void MainWindow::setupModels()
{
    // Налаштування моделі для таблиці файлів
    QStringList headers;
    headers << "Шлях" << "Розмір" << "Тип";
    filesModel->setHorizontalHeaderLabels(headers);
    ui->filesTableView->setModel(filesModel);
    
    // Налаштовуємо розміри колонок
    ui->filesTableView->setColumnWidth(0, 400); // Шлях
    ui->filesTableView->setColumnWidth(1, 100); // Розмір
    ui->filesTableView->setColumnWidth(2, 200); // Тип
}

void MainWindow::on_locateSteamButton_clicked()
{
    QString dir = QFileDialog::getExistingDirectory(this, 
        "Оберіть теку Steam",
        QString(),
        QFileDialog::ShowDirsOnly | QFileDialog::DontResolveSymlinks
    );
    
    if (!dir.isEmpty()) {
        ui->steamFolderEdit->setText(QDir::toNativeSeparators(dir));
    }
}

void MainWindow::on_autoLocateButton_clicked()
{
    QString steamPath = SteamLibrary::findSteamPath();
    if (steamPath.isEmpty()) {
        QMessageBox::warning(this, 
            "Помилка",
            "Не вдалося автоматично знайти теку Steam.\n\n"
            "Типові місця розташування:\n"
            "C:\\Program Files (x86)\\Steam\n"
            "C:\\Program Files\\Steam"
        );
        return;
    }
    
    ui->steamFolderEdit->setText(QDir::toNativeSeparators(steamPath));
    
    // Одразу шукаємо бібліотеки
    QStringList libraries = SteamLibrary::findLibraryFolders(steamPath);
    if (!libraries.isEmpty()) {
        QString message = QString("Знайдено %1 бібліотек(и) Steam:\n").arg(libraries.size());
        for (const QString &lib : libraries) {
            message += "\n" + QDir::toNativeSeparators(lib);
        }
        QMessageBox::information(this, "Інформація", message);
    }
}

void MainWindow::on_searchButton_clicked()
{
    // Очищаємо попередні результати
    filesModel->removeRows(0, filesModel->rowCount());
    
    // Отримуємо шлях до Steam
    QString steamPath = ui->steamFolderEdit->text();
    if (steamPath.isEmpty()) {
        QMessageBox::warning(this, "Помилка", "Спочатку вкажіть теку Steam");
        return;
    }
    
    // Отримуємо всі бібліотеки
    QStringList libraries = SteamLibrary::findLibraryFolders(steamPath);
    if (libraries.isEmpty()) {
        QMessageBox::warning(this, "Помилка", "Не знайдено жодної бібліотеки Steam");
        return;
    }
    
    // Скануємо кожну бібліотеку
    for (const QString &lib : libraries) {
        scanDirectory(lib);
    }
    
    // Сортуємо за розміром
    filesModel->sort(1, Qt::DescendingOrder);
}

void MainWindow::on_removeSelectedButton_clicked()
{
    QModelIndexList selected = ui->filesTableView->selectionModel()->selectedRows();
    
    if (selected.isEmpty()) {
        return;
    }
    
    qint64 totalSize = 0;
    QStringList files;
    
    // Збираємо інформацію про вибрані файли
    for (const QModelIndex &index : selected) {
        QString path = filesModel->data(filesModel->index(index.row(), 0)).toString();
        QString sizeStr = filesModel->data(filesModel->index(index.row(), 1)).toString();
        sizeStr.remove(" MB");
        totalSize += static_cast<qint64>(sizeStr.toDouble() * 1024 * 1024);
        files << QDir::toNativeSeparators(path);
    }
    
    QString message = QString("Ви впевнені, що хочете видалити %1 файл(ів)?\n\n"
                            "Загальний розмір: %2 MB\n\n"
                            "Файли:\n%3")
                             .arg(files.size())
                             .arg(totalSize / 1024.0 / 1024.0, 0, 'f', 2)
                             .arg(files.join("\n"));
    
    QMessageBox::StandardButton reply = QMessageBox::question(this,
        "Підтвердження видалення",
        message,
        QMessageBox::Yes | QMessageBox::No
    );
    
    if (reply == QMessageBox::Yes) {
        int deleted = 0;
        QStringList errors;
        
        // Видаляємо файли
        for (const QString &file : files) {
            QFileInfo fi(file);
            if (fi.isDir()) {
                QDir dir(file);
                if (dir.removeRecursively()) {
                    deleted++;
                } else {
                    errors << file;
                }
            } else {
                QFile f(file);
                if (f.remove()) {
                    deleted++;
                } else {
                    errors << file;
                }
            }
        }
        
        // Оновлюємо модель
        for (const QModelIndex &index : selected) {
            filesModel->removeRow(index.row());
        }
        
        // Показуємо результат
        if (errors.isEmpty()) {
            QMessageBox::information(this, "Інформація", 
                QString("Успішно видалено %1 файл(ів)").arg(deleted));
        } else {
            QMessageBox::warning(this, "Попередження",
                QString("Видалено %1 з %2 файл(ів)\n\n"
                        "Не вдалося видалити:\n%3")
                        .arg(deleted)
                        .arg(files.size())
                        .arg(errors.join("\n")));
        }
    }
}

void MainWindow::scanDirectory(const QString &path)
{
    QDirIterator it(path, QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden, 
                    QDirIterator::Subdirectories);
    
    while (it.hasNext()) {
        QString filePath = it.next();
        QFileInfo fileInfo = it.fileInfo();
        
        // Перевіряємо чи це редистрибутив або інший файл, який можна видалити
        QString type = getFileType(fileInfo);
        if (!type.isEmpty()) {
            QStandardItem *pathItem = new QStandardItem(QDir::toNativeSeparators(filePath));
            
            // Обчислюємо розмір
            qint64 size = getDirSize(filePath);
            QString sizeStr = QString::number(size / 1024.0 / 1024.0, 'f', 2) + " MB";
            QStandardItem *sizeItem = new QStandardItem(sizeStr);
            sizeItem->setData(size, Qt::UserRole); // Для сортування
            
            QStandardItem *typeItem = new QStandardItem(type);
            
            QList<QStandardItem*> row;
            row << pathItem << sizeItem << typeItem;
            filesModel->appendRow(row);
        }
    }
}

qint64 MainWindow::getDirSize(const QString &path)
{
    QFileInfo fi(path);
    if (!fi.isDir()) {
        return fi.size();
    }
    
    qint64 size = 0;
    QDirIterator it(path, QDir::Files | QDir::NoDotAndDotDot | QDir::Hidden, 
                    QDirIterator::Subdirectories);
    
    while (it.hasNext()) {
        it.next();
        size += it.fileInfo().size();
    }
    
    return size;
}

QString MainWindow::getFileType(const QFileInfo &fileInfo)
{
    QString fileName = fileInfo.fileName().toLower();
    QString path = fileInfo.absolutePath().toLower();
    
    // Перевіряємо теки
    if (fileInfo.isDir()) {
        if (fileName == "redist" || fileName == "redistributables" ||
            fileName == "_commonredist" || fileName == "prerequisites") {
            return "Редистрибутиви";
        }
        if (fileName == "directx" || fileName == "vcredist") {
            return "Системні компоненти";
        }
        if (fileName == "support" || fileName == "help") {
            return "Документація";
        }
    }
    // Перевіряємо файли
    else {
        if (fileName.contains("vcredist") || fileName.contains("directx") ||
            fileName.contains("dxsetup") || fileName.contains("dxredist")) {
            return "Системні компоненти";
        }
        if (fileName.endsWith(".pdf") || fileName.endsWith(".txt") ||
            fileName.endsWith(".rtf") || fileName.endsWith(".html")) {
            return "Документація";
        }
    }
    
    return QString();
} 