# Testing the New CLI Authentication System

## Prerequisites

1. **Database Migration**: The `cli_auth_codes` table must be created
2. **Edge Function**: The `cli-auth` function must be deployed
3. **CLI**: The CLI must be built with the new authentication code

## Test Steps

### 1. **Deploy Database Changes**
```bash
cd api
supabase db reset  # This will run the new migration
```

### 2. **Deploy Edge Function**
```bash
supabase functions deploy cli-auth
```

### 3. **Test Authentication Flow**

#### Step 1: Start Login Process
```bash
cd ../cli
./target/release/starthub login
```

**Expected Output:**
```
🔐 Authenticating with Starthub backend...
🌐 API Base: https://api.starthub.so
🔑 Your authentication code: ABC12345
📱 This code will expire in 10 minutes
🌐 Opening browser to: https://editor.starthub.so/cli-auth?code=ABC12345
✅ Browser opened successfully

📋 Please:
1. Copy the authentication code above
2. Paste it in the editor when prompted
3. Complete the authentication in your browser
4. Come back here and paste the code below

? Paste the authentication code: 
```

#### Step 2: Complete Browser Authentication
- Browser should open to `https://editor.starthub.so/cli-auth?code=ABC12345`
- Complete your login in the browser
- Copy the authentication code displayed

#### Step 3: Complete CLI Authentication
- Paste the code back into the CLI
- CLI should validate and complete authentication

**Expected Output:**
```
🔄 Validating authentication code...
✅ Authentication successful!
🔑 Authentication data saved to: /path/to/config/starthub/auth.json
📧 Logged in as: your-email@example.com
```

### 4. **Test Authentication Status**
```bash
./target/release/starthub auth
```

**Expected Output:**
```
🔍 Checking authentication status...
✅ Authenticated with Starthub backend
🌐 API Base: https://api.starthub.so
📧 Email: your-email@example.com
🆔 Profile ID: uuid-here
🔄 Validating authentication...
✅ Authentication is valid and working
```

### 5. **Test Logout**
```bash
./target/release/starthub logout
```

**Expected Output:**
```
🔓 Logging out from Starthub backend...
✅ Successfully logged out!
🗑️  Authentication data removed from: /path/to/config/starthub/auth.json
```

### 6. **Verify Logout**
```bash
./target/release/starthub auth
```

**Expected Output:**
```
🔍 Checking authentication status...
❌ Not authenticated
💡 Use 'starthub login' to authenticate
```

## Test Scenarios

### **Scenario 1: Valid Authentication**
- ✅ Generate code
- ✅ Open browser
- ✅ Complete login
- ✅ Paste code
- ✅ Store authentication

### **Scenario 2: Invalid Code**
- ✅ Generate code
- ✅ Open browser
- ✅ Complete login
- ❌ Paste wrong code
- ❌ Show error message

### **Scenario 3: Expired Code**
- ✅ Generate code
- ⏰ Wait 10+ minutes
- ❌ Try to use expired code
- ❌ Show expiration error

### **Scenario 4: Browser Issues**
- ✅ Generate code
- ❌ Browser doesn't open
- ✅ Manual navigation works
- ✅ Authentication completes

## Debugging

### **Enable Verbose Logging**
```bash
./target/release/starthub login -v
```

### **Check Edge Function Logs**
```bash
cd api
supabase functions logs cli-auth
```

### **Check Database**
```bash
cd api
supabase db reset
supabase db diff
```

## Common Issues

### **1. Migration Failed**
- Ensure Supabase is running locally
- Check migration file syntax
- Verify database connection

### **2. Edge Function Not Deployed**
- Check function exists in config.toml
- Verify function files are present
- Check deployment logs

### **3. Authentication Fails**
- Verify code format (8 characters)
- Check code hasn't expired
- Ensure backend is accessible

### **4. Browser Issues**
- Check webbrowser dependency
- Verify URL format
- Test manual navigation

## Success Criteria

✅ **CLI generates unique codes**  
✅ **Browser opens automatically**  
✅ **Authentication completes in browser**  
✅ **Code validation works**  
✅ **Authentication data is stored**  
✅ **Status command shows authenticated user**  
✅ **Logout removes authentication data**  
✅ **Error handling works for invalid codes**  
✅ **Error handling works for expired codes**  

## Next Steps

After successful testing:

1. **Deploy to staging environment**
2. **Test with real Starthub backend**
3. **Update editor frontend to handle CLI auth flow**
4. **Document user-facing instructions**
5. **Monitor authentication metrics**
