function copyCodebox(button) {
  const codebox = button.closest('.codebox');
  if (!codebox) return;
  const codeElem = codebox.querySelector('.codebox-body code');
  if (!codeElem) return;

  const text = codeElem.innerText;
  navigator.clipboard.writeText(text).then(() => {
    const originalText = button.textContent;
    button.textContent = 'Copied!';
    button.style.color = '#a6e3a1';
    setTimeout(() => {
      button.textContent = originalText;
      button.style.color = '';
    }, 2000);
  }).catch(err => {
    console.error('Failed to copy text: ', err);
  });
}
