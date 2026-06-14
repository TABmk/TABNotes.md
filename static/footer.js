const footerEmoji = document.querySelector("[data-emoji-rotator]");

if (footerEmoji) {
  const emojis = ["❤️", "🧡", "💛", "💚", "🤖"];
  let index = 0;

  window.setInterval(() => {
    index = (index + 1) % emojis.length;
    footerEmoji.textContent = emojis[index];
  }, 5000);
}
