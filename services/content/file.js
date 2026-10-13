const fileName = document.querySelector('#file-name');
const fileKind = document.querySelector('#file-kind');
const filePath = document.querySelector('#file-path');
const openButton = document.querySelector('#open-file');
const fileError = document.querySelector('#file-error');

function showFileError(error) {
  fileError.textContent = error?.message || 'Il file non è disponibile.';
  fileError.hidden = false;
}

async function initializeFile() {
  try {
    const file = await window.orbitFile.getFile();
    fileName.textContent = file.name;
    filePath.textContent = file.path;
    const size = Number(file.size) || 0;
    const sizeLabel = size < 1024 ? `${size} B` : size < 1024 ** 2 ? `${Math.round(size / 1024)} KB` : `${(size / 1024 ** 2).toFixed(1)} MB`;
    fileKind.textContent = `${file.mime || 'File'} · ${sizeLabel}`;
    document.title = file.name;
    openButton.disabled = false;
  } catch (error) {
    fileName.textContent = 'File non disponibile';
    showFileError(error);
  }
}

openButton.addEventListener('click', async () => {
  openButton.disabled = true;
  fileError.hidden = true;
  try {
    await window.orbitFile.openFile();
  } catch (error) {
    showFileError(error);
  } finally {
    openButton.disabled = false;
  }
});

initializeFile();
