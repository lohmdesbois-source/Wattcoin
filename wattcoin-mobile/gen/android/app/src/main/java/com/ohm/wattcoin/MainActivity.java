package com.ohm.wattcoin;

import android.app.NativeActivity;
import android.content.Intent;
import android.content.ClipboardManager;
import android.content.ClipData;
import android.content.Context;
import android.net.Uri;
import android.os.Bundle;
import android.util.Log;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;

public class MainActivity extends NativeActivity {

    // On force Android à lier tes fonctions natives à la librairie Rust
    static {
        System.loadLibrary("wattcoin_wallet"); 
    }

    private static final int GALLERY_REQUEST_CODE = 1001;

    // Le point d'entrée vers Rust (existant)
    public native void onImageBytesReceived(byte[] imageBytes);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
    }

    // Pour pouvoir copier dans le wallet
    public void copyToClipboard(String text) {
        runOnUiThread(() -> {
            try {
                ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
                ClipData clip = ClipData.newPlainText("Wattcoin Address", text);
                if (clipboard != null) {
                    clipboard.setPrimaryClip(clip);
                    Log.i("Wattcoin", "Texte copié dans le presse-papier Android !");
                }
            } catch (Exception e) {
                Log.e("Wattcoin", "Erreur lors de la copie : " + e.getMessage());
            }
        });
    }
	
	
	public String pasteFromClipboard() {
		final String[] result = new String[]{""};
		// Un verrou de synchronisation pour forcer Rust à attendre Java
		final java.util.concurrent.CountDownLatch latch = new java.util.concurrent.CountDownLatch(1);

		// On force l'exécution sur le Thread principal de l'écran Android
		runOnUiThread(new Runnable() {
			@Override
			public void run() {
				android.content.ClipboardManager clipboard = (android.content.ClipboardManager) getSystemService(android.content.Context.CLIPBOARD_SERVICE);
				if (clipboard != null && clipboard.hasPrimaryClip()) {
					android.content.ClipData clip = clipboard.getPrimaryClip();
					if (clip != null && clip.getItemCount() > 0) {
						CharSequence text = clip.getItemAt(0).getText();
						if (text != null) {
							result[0] = text.toString();
						}
					}
				}
				latch.countDown();
			}
		});

		try {
			// Le pont JNI attend patiemment que l'UI ait fini de lire le presse-papier
			latch.await(1, java.util.concurrent.TimeUnit.SECONDS);
		} catch (InterruptedException e) {
			e.printStackTrace();
		}
		
		return result[0];
	}

    public void openGallery() {
        Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
        intent.setType("image/*");
        startActivityForResult(intent, GALLERY_REQUEST_CODE);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        
        if (requestCode == GALLERY_REQUEST_CODE && resultCode == RESULT_OK && data != null) {
            Uri imageUri = data.getData();
            if (imageUri != null) {
                try {
                    InputStream inputStream = getContentResolver().openInputStream(imageUri);
                    ByteArrayOutputStream byteBuffer = new ByteArrayOutputStream();
                    byte[] buffer = new byte[1024];
                    int len;
                    while ((len = inputStream.read(buffer)) != -1) {
                        byteBuffer.write(buffer, 0, len);
                    }
                    byte[] imageBytes = byteBuffer.toByteArray();
                    
                    // On balance tout dans le moteur Rust !
                    onImageBytesReceived(imageBytes);
                    
                } catch (Exception e) {
                    Log.e("Wattcoin", "Erreur lors de la lecture : " + e.getMessage());
                }
            }
        }
    }
}