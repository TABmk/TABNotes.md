const encodeBase64Url = (buffer) => {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
};

const decodeBase64Url = (value) => {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "===".slice((base64.length + 3) % 4);
  const binary = atob(padded);
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
};

const normalizeCreationOptions = (payload) => {
  const publicKey = payload.publicKey || payload;
  return {
    ...publicKey,
    challenge: decodeBase64Url(publicKey.challenge),
    user: {
      ...publicKey.user,
      id: decodeBase64Url(publicKey.user.id),
    },
    excludeCredentials: (publicKey.excludeCredentials || []).map((credential) => ({
      ...credential,
      id: decodeBase64Url(credential.id),
    })),
  };
};

const normalizeRequestOptions = (payload) => {
  const publicKey = payload.publicKey || payload;
  return {
    ...publicKey,
    challenge: decodeBase64Url(publicKey.challenge),
    allowCredentials: (publicKey.allowCredentials || []).map((credential) => ({
      ...credential,
      id: decodeBase64Url(credential.id),
    })),
  };
};

const credentialToJson = (credential) => {
  const response = credential.response;
  const payload = {
    id: credential.id,
    rawId: encodeBase64Url(credential.rawId),
    type: credential.type,
    clientExtensionResults: credential.getClientExtensionResults(),
  };

  if (response.attestationObject) {
    payload.response = {
      attestationObject: encodeBase64Url(response.attestationObject),
      clientDataJSON: encodeBase64Url(response.clientDataJSON),
      transports: response.getTransports ? response.getTransports() : [],
    };
  } else {
    payload.response = {
      authenticatorData: encodeBase64Url(response.authenticatorData),
      clientDataJSON: encodeBase64Url(response.clientDataJSON),
      signature: encodeBase64Url(response.signature),
      userHandle: response.userHandle ? encodeBase64Url(response.userHandle) : null,
    };
  }

  return payload;
};

const setStatus = (target, message, isError = false) => {
  if (!target) {
    return;
  }
  target.textContent = message;
  target.style.color = isError ? "var(--danger)" : "";
};

const passkeyLoginButton = document.querySelector("#passkey-login");
if (passkeyLoginButton) {
  const statusNode = document.querySelector("#passkey-login-status");
  const nextUrl = document.querySelector("[data-next]")?.dataset.next || "/dashboard";

  passkeyLoginButton.addEventListener("click", async () => {
    if (!window.PublicKeyCredential || !navigator.credentials) {
      setStatus(statusNode, "This browser does not support passkeys.", true);
      return;
    }

    try {
      setStatus(statusNode, "Waiting for your authenticator…");
      const startResponse = await fetch("/auth/passkey/start", { method: "POST" });
      if (!startResponse.ok) {
        throw new Error("Unable to start passkey login.");
      }
      const startPayload = await startResponse.json();
      const credential = await navigator.credentials.get({
        publicKey: normalizeRequestOptions(startPayload.options),
      });
      if (!credential) {
        throw new Error("Passkey login was cancelled.");
      }

      const finishResponse = await fetch("/auth/passkey/finish", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          stateId: startPayload.stateId,
          credential: credentialToJson(credential),
        }),
      });
      if (!finishResponse.ok) {
        throw new Error("Passkey verification failed.");
      }
      window.location.href = nextUrl;
    } catch (error) {
      setStatus(statusNode, error.message || "Passkey login failed.", true);
    }
  });
}

const passkeyRegisterForm = document.querySelector("#passkey-register-form");
if (passkeyRegisterForm) {
  const statusNode = document.querySelector("#passkey-register-status");
  passkeyRegisterForm.addEventListener("submit", async (event) => {
    event.preventDefault();

    if (!window.PublicKeyCredential || !navigator.credentials) {
      setStatus(statusNode, "This browser does not support passkeys.", true);
      return;
    }

    const formData = new FormData(passkeyRegisterForm);
    const label = (formData.get("label") || "").toString().trim();
    if (!label) {
      setStatus(statusNode, "Add a label first.", true);
      return;
    }

    try {
      setStatus(statusNode, "Waiting for your authenticator…");
      const startResponse = await fetch("/admin/passkeys/start", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ label }),
      });
      if (!startResponse.ok) {
        throw new Error("Unable to start passkey registration.");
      }

      const startPayload = await startResponse.json();
      const credential = await navigator.credentials.create({
        publicKey: normalizeCreationOptions(startPayload.options),
      });
      if (!credential) {
        throw new Error("Passkey registration was cancelled.");
      }

      const finishResponse = await fetch("/admin/passkeys/finish", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          stateId: startPayload.stateId,
          credential: credentialToJson(credential),
        }),
      });
      if (!finishResponse.ok) {
        throw new Error("Passkey registration failed.");
      }

      window.location.reload();
    } catch (error) {
      setStatus(statusNode, error.message || "Passkey registration failed.", true);
    }
  });
}
