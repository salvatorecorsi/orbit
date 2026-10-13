const parameters = new URLSearchParams(window.location.search);
const imageName = parameters.get('name')?.trim() || 'Immagine';
const imageElement = document.querySelector('#image');
const imageStatus = document.querySelector('#image-status');
const statusMessage = document.querySelector('#status-message');

document.title = imageName;
imageElement.alt = imageName;
document.querySelector('#image-name').textContent = imageName;

function showImageError(message) {
  imageElement.hidden = true;
  imageStatus.hidden = false;
  imageStatus.classList.add('has-error');
  statusMessage.textContent = message;
}

imageElement.addEventListener('load', () => {
  imageElement.hidden = false;
  imageStatus.hidden = true;
});

imageElement.addEventListener('error', () => {
  showImageError('Impossibile caricare questa immagine.');
});

try {
  const imageSource = new URL(parameters.get('source') || '');
  if (!['http:', 'https:', 'file:'].includes(imageSource.protocol)) {
    throw new Error('Unsupported image protocol');
  }
  imageElement.src = imageSource.href;
} catch {
  showImageError('Il percorso dell’immagine non è valido.');
}
