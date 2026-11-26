# Use chatty with your Hive account

With a Hive account, a conversation can leave this computer. Send it to the cloud and it keeps running after you close the laptop. Then open it on another device, answer what it is waiting on, and carry on there or back here.

Without an account, nothing changes: chatty runs offline, keeps everything on this computer and uses the models you set up.

## Sign in

Open **Settings → Extensions** and choose **Sign in** under **Hive Account** (or **Register** to make an account). The sign-in lasts thirty days. Each device signs in on its own.

## What shows up on each device

Conversations in your Hive account appear in the sidebar of every device you are signed in on, including ones you started in the web app. A globe icon marks a conversation that runs in the cloud, and a dot shows when its reply is still being written or is waiting for you.

Once you are signed in, a new conversation is saved to your account after its first reply, so you can open it on the web and your other devices. Its replies still run on this computer, and each new turn is saved to your account when it finishes. To change that:

- **Sync new conversations** (Settings → Extensions, under Hive Account) turns this off. New conversations then stay on this computer.
- **Keep on this device only**, in a conversation's **⋯** menu, stops saving that one conversation. Its copy in your account is left as it is; delete it on the web if you no longer want it there.
- **Sync my existing conversations**, offered once in the same place, saves the conversations you had before you signed in. Nothing older is saved without that.

## Send a conversation to the cloud

In the sidebar, open the conversation's **⋯** menu and choose **Send to the cloud…**. The dialog shows what goes and what stays before anything leaves this computer:

- **Goes with it:** messages, tool activity, usage and attachments.
- **Stays on this computer:** files in your working folder, local MCP servers, your provider keys, memory and skills.

If the conversation uses a model that only runs on this computer, such as an Ollama model, the dialog asks you to pick a cloud model. Your account's default model is already selected. The conversation then runs on the model you picked; the copy on this computer keeps its own model.

Choose **Send to the cloud**. You can close this computer right away: the reply keeps being written in the cloud.

You can't send a conversation while a reply is being written. Wait for it to finish, or stop it, then try again.

## Pick it up on another device

Sign in on the other device. The conversation is in the sidebar; open it to see the reply as it is written, answer an approval or a question, or send the next message. It still runs in the cloud.

To run it on that device instead, open the **⋯** menu and choose **Continue on this device…**. Chatty fetches the latest messages, tool activity and usage, and the next reply runs on that device, with its own models and files. If the cloud's model isn't set up there, the conversation uses one that is. The cloud copy, and any files the cloud run saved, stay in your Hive account.

A conversation is changed in one place at a time. If you send it from a device while another device has it open, or when a sync shows another device has taken it, a line under the conversation says so (for example "running on Work laptop") with a **Continue here** button to take it back.

## Files from a cloud run

Files a cloud run saved appear as file cards on the reply that produced them. Click a card to open it in the artifact panel.

## Resume an interrupted reply

If the cloud restarts while a reply is being written, the conversation is marked **Interrupted** in the sidebar. Open it and choose **Resume** under the conversation to continue the reply. If the server says it is restarting, wait a few seconds and try again.

## Notifications

When a reply finishes in the cloud, or the conversation needs your answer, the desktop app shows a notification, unless you are already looking at that conversation. Click it to open the conversation. You can turn these off in **Settings → General → Notifications**.

## What never leaves your computer

- Files in your working folder. In the cloud, a coding task uses the conversation's GitHub workspace instead.
- Local MCP servers.
- Your provider keys. Replies in the cloud are billed to your Hive account.
- Memory and skills.

## Sign a device out

Open **Settings → Extensions** and choose **Sign out** under **Hive Account**. That device stops getting updates from your account. Conversations already on it stay readable, and the cloud copies stay in your account.
