const fs = require('fs');
const path = require('path');

const bundleDirs = ['deb', 'rpm', 'appimage'];
const basePath = path.join(__dirname, 'src-tauri', 'target', 'release', 'bundle');
const validExtensions = ['.deb', '.rpm', '.AppImage']; 

bundleDirs.forEach(dir => {
  const targetDir = path.join(basePath, dir);
  if (fs.existsSync(targetDir)) {
    const files = fs.readdirSync(targetDir);
    
    // 🧹 ÉTAPE 1 : Le grand ménage (on supprime les anciens fichiers _Linux)
    files.forEach(file => {
      if (file.includes('_Linux')) {
        fs.unlinkSync(path.join(targetDir, file));
        console.log(`🗑️  Ancienne version supprimée : ${file}`);
      }
    });

    // On relit le dossier après avoir fait le ménage
    const updatedFiles = fs.readdirSync(targetDir);
    
    // 🏷️ ÉTAPE 2 : Le renommage des nouveaux fichiers tout frais
    updatedFiles.forEach(file => {
      const filePath = path.join(targetDir, file);
      
      if (fs.statSync(filePath).isFile()) {
        const hasValidExt = validExtensions.some(ext => file.endsWith(ext));
        
        if (file.toLowerCase().includes('wattcoin-wallet') && !file.includes('_Linux') && hasValidExt) {
          const newName = file.replace(/Wattcoin-wallet/i, 'Wattcoin-wallet_Linux');
          fs.renameSync(filePath, path.join(targetDir, newName));
          console.log(`✅ Fichier ${dir.toUpperCase()} renommé : ${newName}`);
        }
      }
    });
  }
});